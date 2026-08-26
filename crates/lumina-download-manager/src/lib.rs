use anyhow::Result;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::fs::OpenOptions as StdOpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tracing::info;
use utoipa::ToSchema;

const DOWNLOAD_USER_AGENT: &str = "lumina-ai-agent";

fn download_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(DOWNLOAD_USER_AGENT)
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(120))
        .build()
}

fn partial_path_for(destination: &Path) -> PathBuf {
    destination.with_extension(
        destination
            .extension()
            .map(|e| format!("{}.part", e.to_string_lossy()))
            .unwrap_or_else(|| "part".to_string()),
    )
}

fn parallel_partial_path_for(destination: &Path) -> PathBuf {
    sibling_path_with_suffix(destination, ".parallel.part")
}

fn sibling_path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut file_name = path.file_name().unwrap_or_default().to_os_string();
    file_name.push(suffix);
    path.with_file_name(file_name)
}

fn parallel_state_dir_for(destination: &Path) -> PathBuf {
    sibling_path_with_suffix(&partial_path_for(destination), ".state")
}

fn lock_path_for(destination: &Path) -> PathBuf {
    sibling_path_with_suffix(&partial_path_for(destination), ".lock")
}

fn range_marker_path(state_dir: &Path, index: usize) -> PathBuf {
    state_dir.join(format!("{index:08}.progress"))
}

struct DestinationLock {
    file: Option<std::fs::File>,
}

impl DestinationLock {
    fn acquire(destination: &Path) -> Result<Self> {
        let path = lock_path_for(destination);
        let file = StdOpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        file.try_lock_exclusive().map_err(|error| {
            anyhow::anyhow!(
                "This model file is already being downloaded by another Lumina process, or its lock cannot be acquired: {} ({})",
                destination.display(),
                error
            )
        })?;
        Ok(Self { file: Some(file) })
    }
}

impl Drop for DestinationLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = FileExt::unlock(&file);
        }
    }
}

#[derive(Clone, Copy)]
struct DownloadTuning {
    parallel_workers: usize,
    parallel_threshold: u64,
    part_size: u64,
}

impl DownloadTuning {
    const DEFAULT_PARALLEL_WORKERS: usize = 4;
    const DEFAULT_PARALLEL_THRESHOLD: u64 = 500 * 1024 * 1024;
    const DEFAULT_PART_SIZE: u64 = 160 * 1024 * 1024;

    fn from_environment() -> Self {
        Self {
            parallel_workers: read_bounded_env_usize(
                "LUMINA_DOWNLOAD_PARALLEL_WORKERS",
                Self::DEFAULT_PARALLEL_WORKERS,
                1,
                16,
            ),
            parallel_threshold: read_bounded_env_u64_megabytes(
                "LUMINA_DOWNLOAD_PARALLEL_THRESHOLD_MB",
                Self::DEFAULT_PARALLEL_THRESHOLD,
                1,
                1024 * 1024,
            ),
            part_size: read_bounded_env_u64_megabytes(
                "LUMINA_DOWNLOAD_PART_SIZE_MB",
                Self::DEFAULT_PART_SIZE,
                1,
                16 * 1024,
            ),
        }
    }
}

fn read_bounded_env_usize(name: &str, default: usize, minimum: usize, maximum: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(minimum, maximum))
        .unwrap_or(default)
}

fn read_bounded_env_u64_megabytes(
    name: &str,
    default_bytes: u64,
    minimum_mb: u64,
    maximum_mb: u64,
) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.clamp(minimum_mb, maximum_mb) * 1024 * 1024)
        .unwrap_or(default_bytes)
}

#[derive(Debug, Clone)]
pub struct DownloadFile {
    pub url: String,
    pub destination: PathBuf,
    pub expected_size: Option<u64>,
    pub expected_sha256: Option<String>,
}

impl DownloadFile {
    pub fn new(url: String, destination: PathBuf, expected_sha256: Option<String>) -> Self {
        Self {
            url,
            destination,
            expected_size: None,
            expected_sha256,
        }
    }

    pub fn verified(
        url: String,
        destination: PathBuf,
        expected_size: u64,
        expected_sha256: Option<String>,
    ) -> Self {
        Self {
            url,
            destination,
            expected_size: (expected_size > 0).then_some(expected_size),
            expected_sha256,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ByteRange {
    index: usize,
    start: u64,
    end: u64,
}

impl ByteRange {
    fn len(self) -> u64 {
        self.end - self.start + 1
    }
}

/// Remove orphaned `.part` files in the given directory (and one level of subdirectories).
/// Preserves `.part` files whose final destination is in `registered_paths` so that
/// in-progress shard downloads can resume after a restart.
pub fn cleanup_partial_downloads(
    dir: &Path,
    registered_paths: &std::collections::HashSet<PathBuf>,
) {
    let should_keep = |part_path: &Path| -> bool {
        let file_name = part_path.file_name().unwrap_or_default().to_string_lossy();
        let final_path = if let Some(final_name) = file_name.strip_suffix(".parallel.part") {
            part_path.with_file_name(final_name)
        } else {
            part_path.with_extension("")
        };
        registered_paths.contains(&final_path)
    };
    let should_keep_state = |state_path: &Path| -> bool {
        let partial_path = state_path.with_extension("");
        let final_path = partial_path.with_extension("");
        registered_paths.contains(&final_path)
    };

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "part") && !should_keep(&path) {
                let _ = std::fs::remove_file(&path);
            }
            if path.is_dir()
                && path.extension().is_some_and(|e| e == "state")
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(".part.state"))
                && !should_keep_state(&path)
            {
                let _ = std::fs::remove_dir_all(&path);
            }
            if path.is_dir() {
                if let Ok(sub_entries) = std::fs::read_dir(&path) {
                    for sub in sub_entries.flatten() {
                        let sub_path = sub.path();
                        if sub_path.extension().is_some_and(|e| e == "part")
                            && !should_keep(&sub_path)
                        {
                            let _ = std::fs::remove_file(&sub_path);
                        }
                        if sub_path.is_dir()
                            && sub_path.extension().is_some_and(|e| e == "state")
                            && sub_path
                                .file_name()
                                .is_some_and(|name| name.to_string_lossy().ends_with(".part.state"))
                            && !should_keep_state(&sub_path)
                        {
                            let _ = std::fs::remove_dir_all(&sub_path);
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DownloadProgress {
    /// Model ID being downloaded
    pub model_id: String,
    /// Download status
    pub status: DownloadStatus,
    /// Bytes downloaded so far
    pub bytes_downloaded: u64,
    /// Total bytes to download
    pub total_bytes: u64,
    /// Download progress percentage (0-100)
    pub progress_percent: f32,
    /// Download speed in bytes per second
    pub speed_bps: Option<u64>,
    /// Estimated time remaining in seconds
    pub eta_seconds: Option<u64>,
    /// Error message if failed
    pub error: Option<String>,
    /// Current automatic retry attempt, if the connection is being retried.
    pub retry_attempt: u32,
    /// Maximum number of automatic retry attempts.
    pub max_retries: u32,
    /// Whether the background download task has exited
    #[serde(skip)]
    pub task_exited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DownloadStatus {
    Downloading,
    Completed,
    Failed,
    Cancelled,
}

type DownloadMap = Arc<Mutex<HashMap<String, DownloadProgress>>>;

pub struct DownloadManager {
    downloads: DownloadMap,
}

impl Default for DownloadManager {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadManager {
    pub fn new() -> Self {
        Self {
            downloads: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn get_progress(&self, model_id: &str) -> Option<DownloadProgress> {
        self.downloads.lock().ok()?.get(model_id).cloned()
    }

    pub fn is_downloading(&self, model_id: &str) -> bool {
        self.get_progress(model_id)
            .is_some_and(|progress| progress.status == DownloadStatus::Downloading)
    }

    pub fn list_progress(&self) -> Vec<DownloadProgress> {
        self.downloads
            .lock()
            .map(|downloads| downloads.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn set_progress(&self, progress: DownloadProgress) {
        if let Ok(mut downloads) = self.downloads.lock() {
            downloads.insert(progress.model_id.clone(), progress);
        }
    }

    pub fn reserve_download(&self, progress: DownloadProgress) -> Result<bool> {
        let mut downloads = self
            .downloads
            .lock()
            .map_err(|_| anyhow::anyhow!("Failed to acquire lock"))?;

        if let Some(existing) = downloads.get(&progress.model_id) {
            if existing.status == DownloadStatus::Downloading
                || (existing.status == DownloadStatus::Cancelled && !existing.task_exited)
            {
                return Ok(false);
            }
        }

        downloads.insert(progress.model_id.clone(), progress);
        Ok(true)
    }

    pub fn update_progress(&self, model_id: &str, update: impl FnOnce(&mut DownloadProgress)) {
        if let Ok(mut downloads) = self.downloads.lock() {
            if let Some(progress) = downloads.get_mut(model_id) {
                update(progress);
            }
        }
    }

    pub fn cancel_download(&self, model_id: &str) -> Result<()> {
        let mut downloads = self
            .downloads
            .lock()
            .map_err(|_| anyhow::anyhow!("Failed to acquire lock"))?;

        if let Some(progress) = downloads.get_mut(model_id) {
            progress.status = DownloadStatus::Cancelled;
            Ok(())
        } else {
            anyhow::bail!("Download not found")
        }
    }

    pub async fn download_model(
        &self,
        model_id: String,
        url: String,
        destination: PathBuf,
        on_complete: Option<Box<dyn FnOnce() + Send + 'static>>,
    ) -> Result<()> {
        self.download_model_sharded(model_id, vec![(url, destination)], 0, on_complete)
            .await
    }

    pub async fn download_model_with_bearer_token(
        &self,
        model_id: String,
        url: String,
        destination: PathBuf,
        bearer_token: Option<String>,
        on_complete: Option<Box<dyn FnOnce() + Send + 'static>>,
    ) -> Result<()> {
        self.download_model_sharded_with_bearer_token(
            model_id,
            vec![(url, destination)],
            0,
            bearer_token,
            on_complete,
        )
        .await
    }

    pub async fn download_model_sharded(
        &self,
        model_id: String,
        files: Vec<(String, PathBuf)>,
        total_size_hint: u64,
        on_complete: Option<Box<dyn FnOnce() + Send + 'static>>,
    ) -> Result<()> {
        self.download_model_sharded_with_bearer_token(
            model_id,
            files,
            total_size_hint,
            None,
            on_complete,
        )
        .await
    }

    pub async fn download_model_sharded_with_bearer_token(
        &self,
        model_id: String,
        files: Vec<(String, PathBuf)>,
        total_size_hint: u64,
        bearer_token: Option<String>,
        on_complete: Option<Box<dyn FnOnce() + Send + 'static>>,
    ) -> Result<()> {
        let files = files
            .into_iter()
            .map(|(url, destination)| DownloadFile::new(url, destination, None))
            .collect();
        self.download_verified_model_sharded_with_bearer_token(
            model_id,
            files,
            total_size_hint,
            bearer_token,
            on_complete,
        )
        .await
    }

    pub async fn download_verified_model_sharded_with_bearer_token(
        &self,
        model_id: String,
        files: Vec<DownloadFile>,
        total_size_hint: u64,
        bearer_token: Option<String>,
        on_complete: Option<Box<dyn FnOnce() + Send + 'static>>,
    ) -> Result<()> {
        info!(model_id = %model_id, file_count = files.len(), "Starting model download");
        {
            let mut downloads = self
                .downloads
                .lock()
                .map_err(|_| anyhow::anyhow!("Failed to acquire lock"))?;

            if let Some(existing) = downloads.get(&model_id) {
                if existing.status == DownloadStatus::Downloading {
                    anyhow::bail!("Download already in progress");
                }
                if existing.status == DownloadStatus::Cancelled && !existing.task_exited {
                    anyhow::bail!(
                        "Download is being cancelled; wait for it to finish before restarting"
                    );
                }
            }

            downloads.insert(
                model_id.clone(),
                DownloadProgress {
                    model_id: model_id.clone(),
                    status: DownloadStatus::Downloading,
                    bytes_downloaded: 0,
                    total_bytes: total_size_hint,
                    progress_percent: 0.0,
                    speed_bps: None,
                    eta_seconds: None,
                    error: None,
                    retry_attempt: 0,
                    max_retries: Self::MAX_RETRIES,
                    task_exited: false,
                },
            );
        }

        // Create parent directories for all files
        for file in &files {
            if let Some(parent) = file.destination.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to create directory: {}", e))?;
            }
        }

        let downloads = self.downloads.clone();
        let model_id_clone = model_id.clone();
        let files_for_cleanup: Vec<PathBuf> =
            files.iter().map(|file| file.destination.clone()).collect();

        tokio::spawn(async move {
            let result = Self::download_files_sequentially(
                &files,
                &downloads,
                &model_id_clone,
                bearer_token.as_deref(),
            )
            .await;

            match result {
                Ok(_) => {
                    info!(model_id = %model_id_clone, "Download completed successfully");
                    if let Ok(mut downloads) = downloads.lock() {
                        if let Some(progress) = downloads.get_mut(&model_id_clone) {
                            progress.status = DownloadStatus::Completed;
                            progress.progress_percent = 100.0;
                            progress.task_exited = true;
                        }
                    }

                    if let Some(callback) = on_complete {
                        callback();
                    }
                }
                Err(e) => {
                    let was_cancelled = Self::is_cancelled(&downloads, &model_id_clone);
                    if was_cancelled {
                        for dest in &files_for_cleanup {
                            Self::remove_partial_artifacts(dest).await;
                        }
                    }

                    if let Ok(mut downloads) = downloads.lock() {
                        if let Some(progress) = downloads.get_mut(&model_id_clone) {
                            if progress.status != DownloadStatus::Cancelled {
                                progress.status = DownloadStatus::Failed;
                            }
                            progress.error = Some(e.to_string());
                            progress.task_exited = true;
                        }
                    }
                }
            }
        });

        Ok(())
    }

    const MAX_RETRIES: u32 = 10;
    const RETRY_BASE_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
    const RETRY_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(60);

    async fn cancellable_sleep(
        delay: std::time::Duration,
        downloads: &DownloadMap,
        model_id: &str,
    ) -> Result<(), anyhow::Error> {
        let check_interval = std::time::Duration::from_millis(500);
        let start = std::time::Instant::now();
        while start.elapsed() < delay {
            if Self::is_cancelled(downloads, model_id) {
                anyhow::bail!("Download cancelled");
            }
            let remaining = delay.saturating_sub(start.elapsed());
            tokio::time::sleep(std::cmp::min(check_interval, remaining)).await;
        }
        Ok(())
    }

    fn is_cancelled(downloads: &DownloadMap, model_id: &str) -> bool {
        if let Ok(downloads) = downloads.lock() {
            if let Some(progress) = downloads.get(model_id) {
                return progress.status == DownloadStatus::Cancelled;
            }
        }
        false
    }

    fn set_retry_progress(downloads: &DownloadMap, model_id: &str, retry_attempt: u32) {
        if let Ok(mut downloads) = downloads.lock() {
            if let Some(progress) = downloads.get_mut(model_id) {
                progress.retry_attempt = retry_attempt;
                progress.max_retries = Self::MAX_RETRIES;
            }
        }
    }

    async fn remove_partial_artifacts(destination: &Path) {
        let _ = tokio::fs::remove_file(partial_path_for(destination)).await;
        let _ = tokio::fs::remove_file(parallel_partial_path_for(destination)).await;
        let _ = tokio::fs::remove_dir_all(parallel_state_dir_for(destination)).await;
    }

    async fn partial_downloaded_bytes(destination: &Path) -> u64 {
        let state_dir = parallel_state_dir_for(destination);
        if state_dir.is_dir() {
            if !parallel_partial_path_for(destination).is_file() {
                return 0;
            }
            let mut total = 0u64;
            if let Ok(mut entries) = tokio::fs::read_dir(state_dir).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    if entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext == "progress")
                    {
                        total = total.saturating_add(
                            Self::read_range_progress(&entry.path()).await.unwrap_or(0),
                        );
                    }
                }
            }
            return total;
        }
        tokio::fs::metadata(partial_path_for(destination))
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0)
    }

    async fn read_range_progress(path: &Path) -> Result<u64> {
        let bytes = tokio::fs::read(path).await?;
        if bytes.len() != std::mem::size_of::<u64>() {
            anyhow::bail!("Invalid range progress marker: {}", path.display());
        }
        let mut value = [0u8; 8];
        value.copy_from_slice(&bytes);
        Ok(u64::from_le_bytes(value))
    }

    async fn write_range_progress(path: &Path, value: u64) -> Result<()> {
        tokio::fs::write(path, value.to_le_bytes()).await?;
        Ok(())
    }

    fn byte_ranges(file_total: u64, part_size: u64) -> Vec<ByteRange> {
        let mut ranges = Vec::new();
        let mut start = 0u64;
        while start < file_total {
            let end = start
                .saturating_add(part_size)
                .saturating_sub(1)
                .min(file_total - 1);
            ranges.push(ByteRange {
                index: ranges.len(),
                start,
                end,
            });
            start = end + 1;
        }
        ranges
    }

    /// Download multiple files sequentially while each large file may use parallel byte ranges.
    async fn download_files_sequentially(
        files: &[DownloadFile],
        downloads: &DownloadMap,
        model_id: &str,
        bearer_token: Option<&str>,
    ) -> Result<(), anyhow::Error> {
        let client = download_client()?;
        let tuning = DownloadTuning::from_environment();
        let total_size_was_provided = downloads
            .lock()
            .ok()
            .and_then(|downloads| {
                downloads
                    .get(model_id)
                    .map(|progress| progress.total_bytes > 0)
            })
            .unwrap_or(false);

        let mut total = 0u64;
        let mut all_resolved = true;
        for file in files {
            let size = if let Some(expected_size) = file.expected_size {
                expected_size
            } else {
                Self::apply_bearer_token(client.head(&file.url), bearer_token)
                    .send()
                    .await
                    .ok()
                    .and_then(|response| Self::response_content_length(&response))
                    .unwrap_or(0)
            };
            if size == 0 {
                all_resolved = false;
            }
            total = total.saturating_add(size);
        }
        if all_resolved && total > 0 {
            if let Ok(mut current) = downloads.lock() {
                if let Some(progress) = current.get_mut(model_id) {
                    progress.total_bytes = total;
                }
            }
        }

        let start_time = std::time::Instant::now();
        let mut cumulative_bytes = 0u64;
        for file in files {
            if let Ok(metadata) = tokio::fs::metadata(&file.destination).await {
                cumulative_bytes = cumulative_bytes.saturating_add(metadata.len());
            } else {
                cumulative_bytes = cumulative_bytes
                    .saturating_add(Self::partial_downloaded_bytes(&file.destination).await);
            }
        }
        let bytes_at_start = cumulative_bytes;
        Self::record_progress(
            downloads,
            model_id,
            cumulative_bytes,
            start_time,
            bytes_at_start,
        );

        for file in files {
            if Self::is_cancelled(downloads, model_id) {
                anyhow::bail!("Download cancelled");
            }
            if file.destination.exists() {
                continue;
            }

            Self::download_one_file(
                &client,
                file,
                downloads,
                model_id,
                &mut cumulative_bytes,
                start_time,
                bytes_at_start,
                bearer_token,
                !total_size_was_provided && !all_resolved,
                tuning,
            )
            .await?;
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_one_file(
        client: &reqwest::Client,
        download: &DownloadFile,
        downloads: &DownloadMap,
        model_id: &str,
        cumulative_bytes: &mut u64,
        start_time: std::time::Instant,
        bytes_at_start: u64,
        bearer_token: Option<&str>,
        add_discovered_size_to_total: bool,
        tuning: DownloadTuning,
    ) -> Result<(), anyhow::Error> {
        let destination = &download.destination;
        let _destination_lock = DestinationLock::acquire(destination)?;
        let state_dir = parallel_state_dir_for(destination);
        let mut file_bytes = Self::partial_downloaded_bytes(destination).await;
        let partial_path = if state_dir.is_dir() {
            parallel_partial_path_for(destination)
        } else {
            partial_path_for(destination)
        };
        let file_total = if let Some(expected_size) = download.expected_size {
            expected_size
        } else {
            Self::apply_bearer_token(client.head(&download.url), bearer_token)
                .send()
                .await
                .ok()
                .and_then(|response| Self::response_content_length(&response))
                .unwrap_or(0)
        };

        if file_total > 0 {
            Self::account_discovered_file_size(
                downloads,
                model_id,
                file_total,
                add_discovered_size_to_total,
            );
        }

        if file_total > 0 && file_bytes == file_total {
            match Self::verify_downloaded_file(
                &partial_path,
                file_total,
                download.expected_sha256.as_deref(),
            )
            .await
            {
                Ok(()) => {
                    if Self::is_cancelled(downloads, model_id) {
                        anyhow::bail!("Download cancelled");
                    }
                    tokio::fs::rename(&partial_path, destination).await?;
                    let _ = tokio::fs::remove_dir_all(&state_dir).await;
                    return Ok(());
                }
                Err(error) => {
                    info!(model_id = %model_id, error = %error, "Completed partial failed integrity validation, re-downloading");
                    *cumulative_bytes = cumulative_bytes.saturating_sub(file_bytes);
                    file_bytes = 0;
                    Self::remove_partial_artifacts(destination).await;
                }
            }
        }

        if file_total > 0 && file_bytes > file_total {
            info!(model_id = %model_id, file_bytes, file_total, "Partial file oversized, re-downloading");
            *cumulative_bytes = cumulative_bytes.saturating_sub(file_bytes);
            file_bytes = 0;
            Self::remove_partial_artifacts(destination).await;
        }

        let use_parallel = file_total >= tuning.parallel_threshold
            && tuning.parallel_workers > 1
            && Self::supports_byte_ranges(client, &download.url, file_total, bearer_token).await;

        if use_parallel {
            return Self::download_one_file_parallel(
                client,
                download,
                file_total,
                file_bytes,
                downloads,
                model_id,
                cumulative_bytes,
                start_time,
                bytes_at_start,
                bearer_token,
                tuning,
            )
            .await;
        }

        if state_dir.exists() {
            *cumulative_bytes = cumulative_bytes.saturating_sub(file_bytes);
            file_bytes = 0;
            Self::remove_partial_artifacts(destination).await;
        }

        Self::download_one_file_single(
            client,
            download,
            file_total,
            file_bytes,
            downloads,
            model_id,
            cumulative_bytes,
            start_time,
            bytes_at_start,
            bearer_token,
            add_discovered_size_to_total,
        )
        .await
    }

    async fn supports_byte_ranges(
        client: &reqwest::Client,
        url: &str,
        expected_total: u64,
        bearer_token: Option<&str>,
    ) -> bool {
        let response =
            Self::apply_bearer_token(client.get(url).header("Range", "bytes=0-0"), bearer_token)
                .send()
                .await;
        let Ok(response) = response else {
            return false;
        };
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return false;
        }
        response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(Self::parse_content_range)
            .is_some_and(|(start, end, total)| start == 0 && end == 0 && total == expected_total)
    }

    fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
        let value = value.strip_prefix("bytes ")?;
        let (range, total) = value.split_once('/')?;
        let (start, end) = range.split_once('-')?;
        Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
    }

    fn response_content_length(response: &reqwest::Response) -> Option<u64> {
        response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .or_else(|| response.content_length())
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_one_file_parallel(
        client: &reqwest::Client,
        download: &DownloadFile,
        file_total: u64,
        previous_file_bytes: u64,
        downloads: &DownloadMap,
        model_id: &str,
        cumulative_bytes: &mut u64,
        start_time: std::time::Instant,
        bytes_at_start: u64,
        bearer_token: Option<&str>,
        tuning: DownloadTuning,
    ) -> Result<(), anyhow::Error> {
        let legacy_partial_path = partial_path_for(&download.destination);
        let partial_path = parallel_partial_path_for(&download.destination);
        let state_dir = parallel_state_dir_for(&download.destination);
        let ranges = Self::byte_ranges(file_total, tuning.part_size);

        for integrity_attempt in 0..=1 {
            let partial_length = tokio::fs::metadata(&partial_path)
                .await
                .map(|metadata| metadata.len())
                .ok();
            let state_already_exists = state_dir.is_dir() && partial_length == Some(file_total);
            if state_dir.is_dir() && !state_already_exists {
                Self::remove_partial_artifacts(&download.destination).await;
            }
            let legacy_prefix = if state_already_exists {
                0
            } else {
                tokio::fs::metadata(&legacy_partial_path)
                    .await
                    .map(|metadata| metadata.len().min(file_total))
                    .unwrap_or(0)
            };
            tokio::fs::create_dir_all(&state_dir).await?;
            if !state_already_exists && legacy_prefix > 0 {
                tokio::fs::rename(&legacy_partial_path, &partial_path).await?;
            }
            let partial = tokio::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&partial_path)
                .await?;
            partial.set_len(file_total).await?;
            drop(partial);

            let mut initial_file_bytes = 0u64;
            for range in &ranges {
                let marker = range_marker_path(&state_dir, range.index);
                let completed = if state_already_exists {
                    Self::read_range_progress(&marker)
                        .await
                        .unwrap_or(0)
                        .min(range.len())
                } else {
                    legacy_prefix.saturating_sub(range.start).min(range.len())
                };
                Self::write_range_progress(&marker, completed).await?;
                initial_file_bytes = initial_file_bytes.saturating_add(completed);
            }

            let counted_before = if integrity_attempt == 0 {
                previous_file_bytes
            } else {
                0
            };
            *cumulative_bytes = cumulative_bytes
                .saturating_sub(counted_before)
                .saturating_add(initial_file_bytes);
            let shared_cumulative = Arc::new(AtomicU64::new(*cumulative_bytes));
            Self::record_progress(
                downloads,
                model_id,
                *cumulative_bytes,
                start_time,
                bytes_at_start,
            );

            info!(
                model_id = %model_id,
                workers = tuning.parallel_workers.min(ranges.len()),
                ranges = ranges.len(),
                resumed_bytes = initial_file_bytes,
                "Starting parallel range download"
            );

            Self::download_ranges(
                client,
                &download.url,
                &partial_path,
                &state_dir,
                &ranges,
                file_total,
                downloads,
                model_id,
                &shared_cumulative,
                start_time,
                bytes_at_start,
                bearer_token,
                tuning.parallel_workers,
            )
            .await?;
            *cumulative_bytes = shared_cumulative.load(Ordering::Relaxed);

            match Self::verify_downloaded_file(
                &partial_path,
                file_total,
                download.expected_sha256.as_deref(),
            )
            .await
            {
                Ok(()) => {
                    if Self::is_cancelled(downloads, model_id) {
                        anyhow::bail!("Download cancelled");
                    }
                    tokio::fs::rename(&partial_path, &download.destination).await?;
                    let _ = tokio::fs::remove_dir_all(&state_dir).await;
                    return Ok(());
                }
                Err(error) if integrity_attempt == 0 => {
                    info!(model_id = %model_id, error = %error, "Parallel download failed integrity validation, retrying from scratch");
                    *cumulative_bytes = cumulative_bytes.saturating_sub(file_total);
                    Self::remove_partial_artifacts(&download.destination).await;
                }
                Err(error) => {
                    *cumulative_bytes = cumulative_bytes.saturating_sub(file_total);
                    Self::remove_partial_artifacts(&download.destination).await;
                    return Err(error);
                }
            }
        }

        unreachable!("integrity retry loop always returns")
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_ranges(
        client: &reqwest::Client,
        url: &str,
        partial_path: &Path,
        state_dir: &Path,
        ranges: &[ByteRange],
        file_total: u64,
        downloads: &DownloadMap,
        model_id: &str,
        cumulative_bytes: &Arc<AtomicU64>,
        start_time: std::time::Instant,
        bytes_at_start: u64,
        bearer_token: Option<&str>,
        requested_workers: usize,
    ) -> Result<()> {
        let worker_count = requested_workers.min(ranges.len()).max(1);
        let mut workers = tokio::task::JoinSet::new();

        for worker_index in 0..worker_count {
            let worker_ranges: Vec<ByteRange> = ranges
                .iter()
                .copied()
                .skip(worker_index)
                .step_by(worker_count)
                .collect();
            let client = client.clone();
            let url = url.to_string();
            let partial_path = partial_path.to_path_buf();
            let state_dir = state_dir.to_path_buf();
            let downloads = downloads.clone();
            let model_id = model_id.to_string();
            let cumulative_bytes = cumulative_bytes.clone();
            let bearer_token = bearer_token.map(str::to_string);

            workers.spawn(async move {
                for range in worker_ranges {
                    Self::download_range_with_retry(
                        &client,
                        &url,
                        &partial_path,
                        &state_dir,
                        range,
                        file_total,
                        &downloads,
                        &model_id,
                        &cumulative_bytes,
                        start_time,
                        bytes_at_start,
                        bearer_token.as_deref(),
                    )
                    .await?;
                }
                Ok::<(), anyhow::Error>(())
            });
        }

        while let Some(result) = workers.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    workers.abort_all();
                    while workers.join_next().await.is_some() {}
                    return Err(error);
                }
                Err(error) => {
                    workers.abort_all();
                    while workers.join_next().await.is_some() {}
                    anyhow::bail!("Parallel download worker failed: {}", error);
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_range_with_retry(
        client: &reqwest::Client,
        url: &str,
        partial_path: &Path,
        state_dir: &Path,
        range: ByteRange,
        file_total: u64,
        downloads: &DownloadMap,
        model_id: &str,
        cumulative_bytes: &Arc<AtomicU64>,
        start_time: std::time::Instant,
        bytes_at_start: u64,
        bearer_token: Option<&str>,
    ) -> Result<()> {
        let marker_path = range_marker_path(state_dir, range.index);
        let mut completed = Self::read_range_progress(&marker_path)
            .await
            .unwrap_or(0)
            .min(range.len());
        let mut retries = 0u32;

        while completed < range.len() {
            if Self::is_cancelled(downloads, model_id) {
                anyhow::bail!("Download cancelled");
            }

            let request_start = range.start + completed;
            let request = Self::apply_bearer_token(
                client
                    .get(url)
                    .header("Range", format!("bytes={request_start}-{}", range.end)),
                bearer_token,
            );
            let response = match request.send().await {
                Ok(response) => response,
                Err(error) => {
                    retries = Self::retry_range_failure(
                        downloads,
                        model_id,
                        retries,
                        format!("connection error: {error}"),
                    )
                    .await?;
                    continue;
                }
            };

            let status = response.status();
            if status != reqwest::StatusCode::PARTIAL_CONTENT {
                let transient = status.is_server_error()
                    || status == reqwest::StatusCode::REQUEST_TIMEOUT
                    || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
                if transient {
                    retries = Self::retry_range_failure(
                        downloads,
                        model_id,
                        retries,
                        format!("HTTP {status}"),
                    )
                    .await?;
                    continue;
                }
                anyhow::bail!(
                    "Server stopped honoring byte ranges for {} (HTTP {})",
                    url,
                    status
                );
            }

            let content_range = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(Self::parse_content_range);
            if content_range != Some((request_start, range.end, file_total)) {
                anyhow::bail!(
                    "Invalid Content-Range for bytes {}-{}: {:?}",
                    request_start,
                    range.end,
                    content_range
                );
            }

            let mut partial = tokio::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(partial_path)
                .await?;
            partial
                .seek(std::io::SeekFrom::Start(request_start))
                .await?;
            let mut marker = tokio::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&marker_path)
                .await?;
            let mut response = response;
            let mut stream_error = None;

            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) => {
                        if Self::is_cancelled(downloads, model_id) {
                            anyhow::bail!("Download cancelled");
                        }
                        let remaining = range.len().saturating_sub(completed);
                        if chunk.len() as u64 > remaining {
                            anyhow::bail!(
                                "Range response exceeded requested end byte {}",
                                range.end
                            );
                        }
                        partial.write_all(&chunk).await?;
                        completed += chunk.len() as u64;
                        marker.seek(std::io::SeekFrom::Start(0)).await?;
                        marker.write_all(&completed.to_le_bytes()).await?;
                        let cumulative = cumulative_bytes
                            .fetch_add(chunk.len() as u64, Ordering::Relaxed)
                            .saturating_add(chunk.len() as u64);
                        Self::record_progress(
                            downloads,
                            model_id,
                            cumulative,
                            start_time,
                            bytes_at_start,
                        );
                    }
                    Ok(None) => break,
                    Err(error) => {
                        stream_error = Some(error.to_string());
                        break;
                    }
                }
            }

            partial.flush().await?;
            marker.flush().await?;
            if completed == range.len() {
                Self::set_retry_progress(downloads, model_id, 0);
                return Ok(());
            }

            retries = Self::retry_range_failure(
                downloads,
                model_id,
                retries,
                stream_error.unwrap_or_else(|| "range response ended early".to_string()),
            )
            .await?;
        }

        Ok(())
    }

    async fn retry_range_failure(
        downloads: &DownloadMap,
        model_id: &str,
        retries: u32,
        reason: String,
    ) -> Result<u32> {
        if retries >= Self::MAX_RETRIES {
            anyhow::bail!(
                "Download range failed after {} retries: {}",
                retries,
                reason
            );
        }
        let next_retry = retries + 1;
        Self::set_retry_progress(downloads, model_id, next_retry);
        let delay = Self::retry_delay(next_retry);
        info!(model_id = %model_id, retry = next_retry, delay_secs = delay.as_secs(), reason = %reason, "Retrying byte range download");
        Self::cancellable_sleep(delay, downloads, model_id).await?;
        Ok(next_retry)
    }

    fn retry_delay(retry: u32) -> std::time::Duration {
        std::cmp::min(
            Self::RETRY_BASE_DELAY * 2u32.saturating_pow(retry.saturating_sub(1)),
            Self::RETRY_MAX_DELAY,
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_one_file_single(
        client: &reqwest::Client,
        download: &DownloadFile,
        mut file_total: u64,
        mut file_bytes: u64,
        downloads: &DownloadMap,
        model_id: &str,
        cumulative_bytes: &mut u64,
        start_time: std::time::Instant,
        bytes_at_start: u64,
        bearer_token: Option<&str>,
        add_discovered_size_to_total: bool,
    ) -> Result<()> {
        let partial_path = partial_path_for(&download.destination);
        let mut retries = 0u32;

        loop {
            if Self::is_cancelled(downloads, model_id) {
                anyhow::bail!("Download cancelled");
            }

            let mut request = Self::apply_bearer_token(client.get(&download.url), bearer_token);
            if file_bytes > 0 {
                request = request.header("Range", format!("bytes={file_bytes}-"));
            }

            let response = match request.send().await {
                Ok(response) => response,
                Err(error) => {
                    retries = Self::retry_range_failure(
                        downloads,
                        model_id,
                        retries,
                        format!("connection error: {error}"),
                    )
                    .await?;
                    continue;
                }
            };

            let status = response.status();
            if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
                if file_total > 0 && file_bytes == file_total {
                    break;
                }
                *cumulative_bytes = cumulative_bytes.saturating_sub(file_bytes);
                file_bytes = 0;
                let _ = tokio::fs::remove_file(&partial_path).await;
                continue;
            }

            if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
                let transient = status.is_server_error()
                    || status == reqwest::StatusCode::REQUEST_TIMEOUT
                    || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
                if !transient {
                    anyhow::bail!("Failed to download: HTTP {}", status);
                }
                retries = Self::retry_range_failure(
                    downloads,
                    model_id,
                    retries,
                    format!("HTTP {status}"),
                )
                .await?;
                continue;
            }

            if file_bytes > 0 && status == reqwest::StatusCode::OK {
                info!(model_id = %model_id, "Server ignored Range header, restarting file from scratch");
                *cumulative_bytes = cumulative_bytes.saturating_sub(file_bytes);
                file_bytes = 0;
                let _ = tokio::fs::remove_file(&partial_path).await;
            }

            if file_total == 0 {
                let discovered = if file_bytes > 0 {
                    response
                        .headers()
                        .get(reqwest::header::CONTENT_RANGE)
                        .and_then(|value| value.to_str().ok())
                        .and_then(Self::parse_content_range)
                        .map(|(_, _, total)| total)
                } else {
                    response.content_length()
                };
                if let Some(total) = discovered {
                    file_total = total;
                    Self::account_discovered_file_size(
                        downloads,
                        model_id,
                        total,
                        add_discovered_size_to_total,
                    );
                }
            }

            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&partial_path)
                .await?;
            if tokio::fs::metadata(&partial_path).await?.len() != file_bytes {
                file.set_len(file_bytes).await?;
            }

            let mut response = response;
            let mut stream_error = None;
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) => {
                        if Self::is_cancelled(downloads, model_id) {
                            anyhow::bail!("Download cancelled");
                        }
                        file.write_all(&chunk).await?;
                        file_bytes = file_bytes.saturating_add(chunk.len() as u64);
                        *cumulative_bytes = cumulative_bytes.saturating_add(chunk.len() as u64);
                        Self::record_progress(
                            downloads,
                            model_id,
                            *cumulative_bytes,
                            start_time,
                            bytes_at_start,
                        );
                    }
                    Ok(None) => break,
                    Err(error) => {
                        stream_error = Some(error.to_string());
                        break;
                    }
                }
            }
            file.flush().await?;

            if stream_error.is_none() && (file_total == 0 || file_bytes == file_total) {
                break;
            }
            retries = Self::retry_range_failure(
                downloads,
                model_id,
                retries,
                stream_error.unwrap_or_else(|| {
                    format!("response ended at {file_bytes} of {file_total} bytes")
                }),
            )
            .await?;
        }

        Self::verify_downloaded_file(
            &partial_path,
            file_total,
            download.expected_sha256.as_deref(),
        )
        .await?;
        if Self::is_cancelled(downloads, model_id) {
            anyhow::bail!("Download cancelled");
        }
        tokio::fs::rename(&partial_path, &download.destination).await?;
        Ok(())
    }

    async fn verify_downloaded_file(
        path: &Path,
        expected_size: u64,
        expected_sha256: Option<&str>,
    ) -> Result<()> {
        let actual_size = tokio::fs::metadata(path).await?.len();
        if expected_size > 0 && actual_size != expected_size {
            anyhow::bail!(
                "Downloaded file size mismatch for {}: expected {}, got {}",
                path.display(),
                expected_size,
                actual_size
            );
        }
        if let Some(expected) = expected_sha256.filter(|value| !value.is_empty()) {
            let actual = Self::sha256_file(path).await?;
            if !actual.eq_ignore_ascii_case(expected) {
                anyhow::bail!(
                    "Downloaded file SHA-256 mismatch for {}: expected {}, got {}",
                    path.display(),
                    expected,
                    actual
                );
            }
        }
        Ok(())
    }

    async fn sha256_file(path: &Path) -> Result<String> {
        let mut file = tokio::fs::File::open(path).await?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let mut encoded = String::with_capacity(64);
        for byte in hasher.finalize() {
            write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
        }
        Ok(encoded)
    }

    fn record_progress(
        downloads: &DownloadMap,
        model_id: &str,
        cumulative_bytes: u64,
        start_time: std::time::Instant,
        bytes_at_start: u64,
    ) {
        let elapsed = start_time.elapsed().as_secs_f64();
        let session_bytes = cumulative_bytes.saturating_sub(bytes_at_start);
        let speed_bps = (elapsed > 0.0).then(|| (session_bytes as f64 / elapsed) as u64);

        if let Ok(mut current) = downloads.lock() {
            if let Some(progress) = current.get_mut(model_id) {
                let total = progress.total_bytes;
                progress.bytes_downloaded = if total > 0 {
                    cumulative_bytes.min(total)
                } else {
                    cumulative_bytes
                };
                progress.progress_percent = if total > 0 {
                    (cumulative_bytes.min(total) as f64 / total as f64 * 100.0) as f32
                } else {
                    0.0
                };
                progress.speed_bps = speed_bps;
                progress.eta_seconds = speed_bps
                    .filter(|speed| *speed > 0 && total > 0)
                    .map(|speed| total.saturating_sub(cumulative_bytes) / speed);
                progress.retry_attempt = 0;
            }
        }
    }

    pub fn clear_completed(&self, model_id: &str) {
        if let Ok(mut downloads) = self.downloads.lock() {
            if let Some(progress) = downloads.get(model_id) {
                let is_terminal = progress.status == DownloadStatus::Completed
                    || progress.status == DownloadStatus::Failed
                    || progress.status == DownloadStatus::Cancelled;
                if is_terminal && progress.task_exited {
                    downloads.remove(model_id);
                }
            }
        }
    }

    fn apply_bearer_token(
        request: reqwest::RequestBuilder,
        bearer_token: Option<&str>,
    ) -> reqwest::RequestBuilder {
        if let Some(token) = bearer_token.filter(|token| !token.is_empty()) {
            request.header("Authorization", format!("Bearer {}", token))
        } else {
            request
        }
    }

    fn account_discovered_file_size(
        downloads: &DownloadMap,
        model_id: &str,
        size: u64,
        should_add: bool,
    ) {
        if !should_add {
            return;
        }
        if let Ok(mut downloads) = downloads.lock() {
            if let Some(progress) = downloads.get_mut(model_id) {
                progress.total_bytes = progress.total_bytes.saturating_add(size);
            }
        }
    }
}

static DOWNLOAD_MANAGER: once_cell::sync::Lazy<DownloadManager> =
    once_cell::sync::Lazy::new(DownloadManager::new);

pub fn get_download_manager() -> &'static DownloadManager {
    &DOWNLOAD_MANAGER
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    struct RangeTestServer {
        url: String,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
        max_active_gets: Arc<AtomicUsize>,
        bytes_served: Arc<AtomicU64>,
        head_requests: Arc<AtomicUsize>,
    }

    impl RangeTestServer {
        fn start(data: Vec<u8>, honor_ranges: bool, response_delay: std::time::Duration) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let max_active_gets = Arc::new(AtomicUsize::new(0));
            let active_gets = Arc::new(AtomicUsize::new(0));
            let bytes_served = Arc::new(AtomicU64::new(0));
            let head_requests = Arc::new(AtomicUsize::new(0));
            let data = Arc::new(data);

            let server_stop = stop.clone();
            let server_max_active = max_active_gets.clone();
            let server_bytes_served = bytes_served.clone();
            let server_head_requests = head_requests.clone();
            let thread = std::thread::spawn(move || {
                while let Ok((stream, _)) = listener.accept() {
                    if server_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let data = data.clone();
                    let active_gets = active_gets.clone();
                    let max_active_gets = server_max_active.clone();
                    let bytes_served = server_bytes_served.clone();
                    let head_requests = server_head_requests.clone();
                    std::thread::spawn(move || {
                        Self::handle_request(
                            stream,
                            &data,
                            honor_ranges,
                            response_delay,
                            &active_gets,
                            &max_active_gets,
                            &bytes_served,
                            &head_requests,
                        );
                    });
                }
            });

            Self {
                url: format!("http://{address}/model.gguf"),
                stop,
                thread: Some(thread),
                max_active_gets,
                bytes_served,
                head_requests,
            }
        }

        #[allow(clippy::too_many_arguments)]
        fn handle_request(
            mut stream: std::net::TcpStream,
            data: &[u8],
            honor_ranges: bool,
            response_delay: std::time::Duration,
            active_gets: &AtomicUsize,
            max_active_gets: &AtomicUsize,
            bytes_served: &AtomicU64,
            head_requests: &AtomicUsize,
        ) {
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let Ok(read) = stream.read(&mut buffer) else {
                    return;
                };
                if read == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }

            let request = String::from_utf8_lossy(&request);
            let method = request.split_whitespace().next().unwrap_or_default();
            if method == "HEAD" {
                head_requests.fetch_add(1, Ordering::SeqCst);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                    data.len()
                );
                let _ = stream.write_all(response.as_bytes());
                return;
            }

            let active = active_gets.fetch_add(1, Ordering::SeqCst) + 1;
            max_active_gets.fetch_max(active, Ordering::SeqCst);
            std::thread::sleep(response_delay);

            let requested_range = request.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("range")
                    .then(|| value.trim().strip_prefix("bytes="))
                    .flatten()
                    .and_then(|value| value.split_once('-'))
                    .and_then(|(start, end)| {
                        Some((
                            start.parse::<usize>().ok()?,
                            if end.is_empty() {
                                data.len().checked_sub(1)?
                            } else {
                                end.parse::<usize>().ok()?
                            },
                        ))
                    })
            });

            let (status, body_start, body_end) = if honor_ranges {
                if let Some((start, end)) = requested_range {
                    ("206 Partial Content", start, end.min(data.len() - 1))
                } else {
                    ("200 OK", 0, data.len() - 1)
                }
            } else {
                ("200 OK", 0, data.len() - 1)
            };
            let body = &data[body_start..=body_end];
            let content_range = if status.starts_with("206") {
                format!(
                    "Content-Range: bytes {body_start}-{body_end}/{}\r\n",
                    data.len()
                )
            } else {
                String::new()
            };
            let headers = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{content_range}Connection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(headers.as_bytes());
            let _ = stream.write_all(body);
            bytes_served.fetch_add(body.len() as u64, Ordering::SeqCst);
            active_gets.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl Drop for RangeTestServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Ok(address) = self
                .url
                .strip_prefix("http://")
                .and_then(|value| value.split('/').next())
                .unwrap_or_default()
                .parse::<std::net::SocketAddr>()
            {
                let _ = std::net::TcpStream::connect(address);
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn download_progress(model_id: &str, total_bytes: u64) -> DownloadMap {
        let downloads = Arc::new(Mutex::new(HashMap::new()));
        downloads.lock().unwrap().insert(
            model_id.to_string(),
            DownloadProgress {
                model_id: model_id.to_string(),
                status: DownloadStatus::Downloading,
                bytes_downloaded: 0,
                total_bytes,
                progress_percent: 0.0,
                speed_bps: None,
                eta_seconds: None,
                error: None,
                retry_attempt: 0,
                max_retries: DownloadManager::MAX_RETRIES,
                task_exited: false,
            },
        );
        downloads
    }

    fn sha256_bytes(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let mut encoded = String::with_capacity(64);
        for byte in hasher.finalize() {
            write!(&mut encoded, "{byte:02x}").unwrap();
        }
        encoded
    }

    #[test]
    fn modelscope_downloads_send_a_user_agent() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 4096];
            let bytes_read = stream.read(&mut buffer).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
            String::from_utf8_lossy(&buffer[..bytes_read]).to_string()
        });

        let client = download_client().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            client
                .get(format!("http://{address}/model.gguf"))
                .send()
                .await
                .unwrap();
        });
        let request = server.join().unwrap().to_ascii_lowercase();

        assert!(request.contains(&format!("user-agent: {DOWNLOAD_USER_AGENT}")));
    }

    #[test]
    fn provided_total_is_not_double_counted() {
        let manager = DownloadManager::new();
        manager.set_progress(DownloadProgress {
            model_id: "model".to_string(),
            status: DownloadStatus::Downloading,
            bytes_downloaded: 0,
            total_bytes: 101,
            progress_percent: 0.0,
            speed_bps: None,
            eta_seconds: None,
            error: None,
            retry_attempt: 0,
            max_retries: DownloadManager::MAX_RETRIES,
            task_exited: false,
        });

        DownloadManager::account_discovered_file_size(&manager.downloads, "model", 101, false);

        assert_eq!(manager.get_progress("model").unwrap().total_bytes, 101);
    }

    #[test]
    fn destination_lock_blocks_a_second_downloader() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("model.gguf");
        let first = DestinationLock::acquire(&destination).unwrap();

        let error = match DestinationLock::acquire(&destination) {
            Ok(_) => panic!("second lock unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("another Lumina process"));

        drop(first);
        DestinationLock::acquire(&destination).unwrap();
    }

    #[test]
    fn parallel_partial_uses_a_rollback_safe_filename() {
        let destination = PathBuf::from("model.gguf");

        assert_eq!(
            partial_path_for(&destination),
            PathBuf::from("model.gguf.part")
        );
        assert_eq!(
            parallel_partial_path_for(&destination),
            PathBuf::from("model.gguf.parallel.part")
        );
    }

    #[test]
    fn parallel_ranges_resume_legacy_partial_and_verify_sha256() {
        let data: Vec<u8> = (0..1024 * 1024).map(|index| (index % 251) as u8).collect();
        let server =
            RangeTestServer::start(data.clone(), true, std::time::Duration::from_millis(40));
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("model.gguf");
        let partial = partial_path_for(&destination);
        let legacy_prefix = 300_000usize;
        std::fs::write(&partial, &data[..legacy_prefix]).unwrap();

        let downloads = download_progress("parallel", data.len() as u64);
        let download = DownloadFile::verified(
            server.url.clone(),
            destination.clone(),
            data.len() as u64,
            Some(sha256_bytes(&data)),
        );
        let client = download_client().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut cumulative = legacy_prefix as u64;
        assert!(runtime.block_on(DownloadManager::supports_byte_ranges(
            &client,
            &download.url,
            data.len() as u64,
            None,
        )));
        runtime
            .block_on(DownloadManager::download_one_file(
                &client,
                &download,
                &downloads,
                "parallel",
                &mut cumulative,
                std::time::Instant::now(),
                legacy_prefix as u64,
                None,
                false,
                DownloadTuning {
                    parallel_workers: 4,
                    parallel_threshold: 1,
                    part_size: 128 * 1024,
                },
            ))
            .unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), data);
        assert!(!partial_path_for(&destination).exists());
        assert!(!parallel_partial_path_for(&destination).exists());
        assert!(!parallel_state_dir_for(&destination).exists());
        assert!(server.max_active_gets.load(Ordering::SeqCst) >= 2);
        assert_eq!(server.head_requests.load(Ordering::SeqCst), 0);
        assert!(
            server.bytes_served.load(Ordering::SeqCst)
                < data.len() as u64 - legacy_prefix as u64 + 1024
        );
        let progress = downloads.lock().unwrap()["parallel"].clone();
        assert_eq!(progress.bytes_downloaded, data.len() as u64);
        assert_eq!(progress.progress_percent, 100.0);
    }

    #[test]
    fn falls_back_to_single_stream_when_server_ignores_ranges() {
        let data: Vec<u8> = (0..256 * 1024).map(|index| (index % 239) as u8).collect();
        let server =
            RangeTestServer::start(data.clone(), false, std::time::Duration::from_millis(5));
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("model.gguf");
        let downloads = download_progress("fallback", data.len() as u64);
        let download = DownloadFile::new(server.url.clone(), destination.clone(), None);
        let client = download_client().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut cumulative = 0u64;
        runtime
            .block_on(DownloadManager::download_one_file(
                &client,
                &download,
                &downloads,
                "fallback",
                &mut cumulative,
                std::time::Instant::now(),
                0,
                None,
                false,
                DownloadTuning {
                    parallel_workers: 4,
                    parallel_threshold: 1,
                    part_size: 64 * 1024,
                },
            ))
            .unwrap();

        assert_eq!(std::fs::read(destination).unwrap(), data);
        assert_eq!(server.max_active_gets.load(Ordering::SeqCst), 1);
    }
}
