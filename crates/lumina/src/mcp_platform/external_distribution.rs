use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::sync::CancellationToken;

use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::manifest::PermissionKind;

pub const EXTERNAL_DISTRIBUTION_ADAPTER_VERSION: &str = "1";
const MAX_EXTERNAL_PROCESS_STREAM_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectProcessRequest {
    pub executable: PathBuf,
    pub argv: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub stdin: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectProcessOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[async_trait]
pub trait DirectProcessRunner: Send + Sync {
    async fn run(
        &self,
        request: DirectProcessRequest,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<DirectProcessOutput>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TokioDirectProcessRunner;

#[async_trait]
impl DirectProcessRunner for TokioDirectProcessRunner {
    async fn run(
        &self,
        request: DirectProcessRequest,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<DirectProcessOutput> {
        let mut command = tokio::process::Command::new(&request.executable);
        command
            .args(&request.argv)
            .env_clear()
            .envs(&request.environment)
            .stdin(if request.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &request.cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().map_err(|_| process_unavailable())?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        if stdout.is_none() || stderr.is_none() {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(process_unavailable());
        }
        let stdout = stdout.expect("stdout presence checked");
        let stderr = stderr.expect("stderr presence checked");
        let reader_failure = CancellationToken::new();
        let stdout_failure = reader_failure.clone();
        let stdout_task =
            tokio::spawn(async move { read_bounded_process_stream(stdout, stdout_failure).await });
        let stderr_failure = reader_failure.clone();
        let stderr_task =
            tokio::spawn(async move { read_bounded_process_stream(stderr, stderr_failure).await });
        let stdin_result = if let Some(stdin) = request.stdin {
            let Some(mut pipe) = child.stdin.take() else {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(process_unavailable());
            };
            tokio::select! {
                _ = cancellation.cancelled() => Err(cancelled()),
                _ = reader_failure.cancelled() => Err(process_unavailable()),
                result = async {
                    pipe.write_all(&stdin).await.map_err(|_| process_unavailable())?;
                    pipe.shutdown().await.map_err(|_| process_unavailable())
                } => result,
            }
        } else {
            Ok(())
        };
        if let Err(error) = stdin_result {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            return Err(error);
        }
        enum ProcessCompletion {
            Exited(std::io::Result<std::process::ExitStatus>),
            Cancelled,
            ReaderFailed,
        }
        let completion = tokio::select! {
            _ = cancellation.cancelled() => ProcessCompletion::Cancelled,
            _ = reader_failure.cancelled() => ProcessCompletion::ReaderFailed,
            status = child.wait() => ProcessCompletion::Exited(status),
        };
        let completion = match completion {
            ProcessCompletion::Exited(Ok(status)) => ProcessCompletion::Exited(Ok(status)),
            ProcessCompletion::Exited(Err(error)) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                ProcessCompletion::Exited(Err(error))
            }
            other => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                other
            }
        };
        let stdout_result = stdout_task.await;
        let stderr_result = stderr_task.await;
        if matches!(completion, ProcessCompletion::Cancelled) {
            return Err(cancelled());
        }
        let ProcessCompletion::Exited(status_result) = completion else {
            return Err(process_unavailable());
        };
        let status = status_result.map_err(|_| process_unavailable())?;
        let stdout = stdout_result
            .map_err(|_| process_unavailable())?
            .map_err(|_| process_unavailable())?;
        let stderr = stderr_result
            .map_err(|_| process_unavailable())?
            .map_err(|_| process_unavailable())?;
        Ok(DirectProcessOutput {
            success: status.success(),
            stdout,
            stderr,
        })
    }
}

async fn read_bounded_process_stream<R>(
    reader: R,
    failure: CancellationToken,
) -> Result<Vec<u8>, ()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut value = Vec::new();
    let result = reader
        .take(MAX_EXTERNAL_PROCESS_STREAM_BYTES + 1)
        .read_to_end(&mut value)
        .await;
    if result.is_err() || value.len() as u64 > MAX_EXTERNAL_PROCESS_STREAM_BYTES {
        failure.cancel();
        return Err(());
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMountGrant {
    pub opaque_grant_id: String,
    pub server_path: PathBuf,
    pub kind: PermissionKind,
}

pub trait MountPermissionResolver: Send + Sync {
    fn resolve(&self, permission_id: &str) -> McpPlatformResult<ResolvedMountGrant>;
}

#[async_trait]
pub trait PublicNetworkResolver: Send + Sync {
    async fn resolve(&self, host: &str, port: u16) -> McpPlatformResult<Vec<std::net::SocketAddr>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemPublicNetworkResolver;

#[async_trait]
impl PublicNetworkResolver for SystemPublicNetworkResolver {
    async fn resolve(&self, host: &str, port: u16) -> McpPlatformResult<Vec<std::net::SocketAddr>> {
        super::managed_distribution::resolve_public_addresses(host, port).await
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DenyMountPermissionResolver;

impl MountPermissionResolver for DenyMountPermissionResolver {
    fn resolve(&self, _permission_id: &str) -> McpPlatformResult<ResolvedMountGrant> {
        Err(McpPlatformError::new(
            McpPlatformErrorCode::MountPermissionDenied,
            "filesystem permission grant is unavailable",
        ))
    }
}

#[derive(Debug, Clone)]
pub struct DockerToolCapability {
    executable: PathBuf,
    pub policy_allowed: bool,
}

impl DockerToolCapability {
    #[cfg(test)]
    pub fn for_test(
        executable: PathBuf,
        _daemon_version: impl Into<String>,
        _rootless: bool,
        policy_allowed: bool,
    ) -> Self {
        Self {
            executable,
            policy_allowed,
        }
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }
}

#[derive(Debug, Clone)]
pub struct GitToolCapability {
    executable: PathBuf,
}

impl GitToolCapability {
    #[cfg(test)]
    pub fn for_test(executable: PathBuf) -> Self {
        Self { executable }
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExternalToolCapabilities {
    docker: Option<DockerToolCapability>,
    git: Option<GitToolCapability>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ExternalCapabilitySnapshot {
    pub docker_available: bool,
    pub docker_daemon_verified: bool,
    pub docker_policy_allowed: bool,
    pub git_available: bool,
}

impl ExternalToolCapabilities {
    pub fn discover_with_docker_policy(policy_allowed: bool) -> Self {
        let docker = resolve_executable(if cfg!(windows) {
            "docker.exe"
        } else {
            "docker"
        })
        .map(|executable| DockerToolCapability {
            executable,
            policy_allowed,
        });
        let git = resolve_executable(if cfg!(windows) { "git.exe" } else { "git" })
            .map(|executable| GitToolCapability { executable });
        Self { docker, git }
    }

    #[cfg(test)]
    pub fn for_test(docker: Option<DockerToolCapability>, git: Option<GitToolCapability>) -> Self {
        Self { docker, git }
    }

    pub fn snapshot(&self) -> ExternalCapabilitySnapshot {
        ExternalCapabilitySnapshot {
            docker_available: self.docker.is_some(),
            docker_daemon_verified: false,
            docker_policy_allowed: self
                .docker
                .as_ref()
                .is_none_or(|capability| capability.policy_allowed),
            git_available: self.git.is_some(),
        }
    }

    pub fn docker(&self) -> McpPlatformResult<&DockerToolCapability> {
        self.docker.as_ref().ok_or_else(|| {
            McpPlatformError::new(
                McpPlatformErrorCode::DockerUnavailable,
                "Docker capability is unavailable",
            )
        })
    }

    pub fn git(&self) -> McpPlatformResult<&GitToolCapability> {
        self.git.as_ref().ok_or_else(|| {
            McpPlatformError::new(
                McpPlatformErrorCode::GitUnavailable,
                "Git capability is unavailable",
            )
        })
    }
}

#[derive(Clone)]
pub struct ExternalDistributionPorts {
    pub process: Arc<dyn DirectProcessRunner>,
    pub mount_permissions: Arc<dyn MountPermissionResolver>,
    pub capabilities: ExternalToolCapabilities,
    pub network: Arc<dyn PublicNetworkResolver>,
}

impl ExternalDistributionPorts {
    pub fn production() -> Self {
        Self::production_with_docker_policy(true)
    }

    pub fn production_with_docker_policy(policy_allowed: bool) -> Self {
        Self {
            process: Arc::new(TokioDirectProcessRunner),
            mount_permissions: Arc::new(DenyMountPermissionResolver),
            capabilities: ExternalToolCapabilities::discover_with_docker_policy(policy_allowed),
            network: Arc::new(SystemPublicNetworkResolver),
        }
    }

    pub fn snapshot(&self) -> ExternalCapabilitySnapshot {
        self.capabilities.snapshot()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SupplyChainEvidence {
    Docker {
        image: String,
        image_digest: String,
        adapter_version: String,
        daemon_version: String,
        rootless: bool,
        mount_plan_digest: String,
        created_at_ms: i64,
    },
    GitDev {
        repository_origin: String,
        commit: String,
        git_tree_id: String,
        materialized_tree_digest: String,
        adapter_version: String,
        created_at_ms: i64,
    },
}

impl SupplyChainEvidence {
    pub(crate) fn same_authority(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Docker {
                    image,
                    image_digest,
                    adapter_version,
                    daemon_version,
                    rootless,
                    mount_plan_digest,
                    ..
                },
                Self::Docker {
                    image: other_image,
                    image_digest: other_digest,
                    adapter_version: other_adapter,
                    daemon_version: other_daemon,
                    rootless: other_rootless,
                    mount_plan_digest: other_mounts,
                    ..
                },
            ) => {
                image == other_image
                    && image_digest == other_digest
                    && adapter_version == other_adapter
                    && daemon_version == other_daemon
                    && rootless == other_rootless
                    && mount_plan_digest == other_mounts
            }
            (
                Self::GitDev {
                    repository_origin,
                    commit,
                    git_tree_id,
                    materialized_tree_digest,
                    adapter_version,
                    ..
                },
                Self::GitDev {
                    repository_origin: other_origin,
                    commit: other_commit,
                    git_tree_id: other_tree,
                    materialized_tree_digest: other_materialized,
                    adapter_version: other_adapter,
                    ..
                },
            ) => {
                repository_origin == other_origin
                    && commit == other_commit
                    && git_tree_id == other_tree
                    && materialized_tree_digest == other_materialized
                    && adapter_version == other_adapter
            }
            _ => false,
        }
    }
}

fn resolve_executable(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find_map(|candidate| {
            let metadata = std::fs::metadata(&candidate).ok()?;
            if metadata.is_file() {
                std::fs::canonicalize(candidate).ok()
            } else {
                None
            }
        })
}

const fn process_unavailable() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RepositoryUnavailable,
        "external process capability failed",
    )
}

const fn cancelled() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::TaskNotCancellable,
        "external process was cancelled at a safe boundary",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn output_command(stderr: bool) -> (PathBuf, Vec<String>) {
        if cfg!(windows) {
            let redirect = if stderr { " 1>&2" } else { "" };
            (
                PathBuf::from(std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into())),
                vec![
                    "/D".to_string(),
                    "/S".to_string(),
                    "/C".to_string(),
                    format!(
                        "for /L %i in (1,1,1000000) do @echo 0123456789abcdef0123456789abcdef{redirect}"
                    ),
                ],
            )
        } else {
            let redirect = if stderr { " >&2" } else { "" };
            (
                PathBuf::from("/bin/sh"),
                vec![
                    "-c".to_string(),
                    format!(
                        "/usr/bin/head -c {} /dev/zero{redirect}",
                        MAX_EXTERNAL_PROCESS_STREAM_BYTES + 1
                    ),
                ],
            )
        }
    }

    async fn assert_output_limit(stderr: bool) {
        let (executable, argv) = output_command(stderr);
        let error = tokio::time::timeout(
            Duration::from_secs(10),
            TokioDirectProcessRunner.run(
                DirectProcessRequest {
                    executable,
                    argv,
                    environment: BTreeMap::new(),
                    cwd: None,
                    stdin: None,
                },
                &CancellationToken::new(),
            ),
        )
        .await
        .expect("oversized output must terminate and reap the process")
        .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RepositoryUnavailable);
        assert_eq!(error.message(), "external process capability failed");
    }

    #[tokio::test]
    async fn direct_runner_reaps_process_when_stdout_exceeds_limit() {
        assert_output_limit(false).await;
    }

    #[tokio::test]
    async fn direct_runner_reaps_process_when_stderr_exceeds_limit() {
        assert_output_limit(true).await;
    }

    #[tokio::test]
    async fn direct_runner_cancellation_joins_continuous_stdout_and_stderr() {
        let (executable, argv) = if cfg!(windows) {
            (
                PathBuf::from(std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into())),
                vec![
                    "/D".to_string(),
                    "/S".to_string(),
                    "/C".to_string(),
                    "for /L %i in (1,1,1000000) do @echo out%i & @echo err%i 1>&2".to_string(),
                ],
            )
        } else {
            (
                PathBuf::from("/bin/sh"),
                vec![
                    "-c".to_string(),
                    "while :; do echo out; echo err >&2; sleep 0.01; done".to_string(),
                ],
            )
        };
        let cancellation = CancellationToken::new();
        let signal = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            signal.cancel();
        });
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            TokioDirectProcessRunner.run(
                DirectProcessRequest {
                    executable,
                    argv,
                    environment: BTreeMap::new(),
                    cwd: None,
                    stdin: None,
                },
                &cancellation,
            ),
        )
        .await
        .expect("runner cancellation must wait for both reader tasks")
        .unwrap_err();
        assert_eq!(result.code(), McpPlatformErrorCode::TaskNotCancellable);
    }
}
