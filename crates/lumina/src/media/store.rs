use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use sqlx::{Pool, Row, Sqlite};

use super::domain::{self, *};
use super::files::{self, GuardedDirectory};
use super::formats;

#[derive(Clone)]
pub struct MediaResultStore {
    pool: Pool<Sqlite>,
}

#[derive(Debug)]
pub enum BeginMediaPublish {
    Existing(MediaResultRef),
    Staging(StagedMedia),
}

#[derive(Debug)]
pub struct StagedMedia {
    request: PublishMediaRequest,
    created_at: i64,
    file: File,
    identity: String,
    directory: GuardedDirectory,
    _root: GuardedDirectory,
    written: u64,
}

impl Write for StagedMedia {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > MAX_MEDIA_BYTES.saturating_sub(self.written) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Media publication exceeds the byte limit",
            ));
        }
        let count = self.file.write(bytes)?;
        self.written += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

pub struct VerifiedMediaContent {
    result: MediaResultRef,
    path: PathBuf,
    file: File,
    _directory: GuardedDirectory,
    _root: GuardedDirectory,
}

impl VerifiedMediaContent {
    pub fn result(&self) -> &MediaResultRef {
        &self.result
    }

    /// The guard must remain alive through a desktop action using this path.
    /// Preview and save-as should read this handle rather than reopen the path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read_range(&mut self, offset: u64, length: usize) -> Result<Vec<u8>> {
        ensure!(
            length > 0 && length <= MAX_MEDIA_READ_BYTES && offset < self.result.bytes,
            "Media content range is outside its bounds"
        );
        let count = usize::try_from((self.result.bytes - offset).min(length as u64))?;
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(MediaAccessError::io)?;
        let mut bytes = vec![0; count];
        self.file
            .read_exact(&mut bytes)
            .map_err(MediaAccessError::io)?;
        Ok(bytes)
    }
}

struct PreparedMedia {
    staged: StagedMedia,
    result: MediaResultRef,
}

struct Record {
    request: PublishMediaRequest,
    request_digest: String,
    created_at: i64,
    state: String,
    identity: Option<String>,
    result: Option<MediaResultRef>,
}

fn relative_directory(request: &PublishMediaRequest) -> String {
    format!(
        "{}/{}",
        domain::segment(&request.session_id),
        domain::segment(&request.job_id)
    )
}

fn stage_name(result_id: &str) -> String {
    format!("{result_id}.pending")
}

fn ownership(source: &MediaSource) -> MediaOwnership {
    match source {
        MediaSource::LocalModel { .. } => MediaOwnership::ApplicationGenerated,
        MediaSource::Mcp { .. } => MediaOwnership::ApplicationImported,
    }
}

fn tampered() -> anyhow::Error {
    MediaAccessError::new(
        MediaAvailability::Tampered,
        "Published media identity or content has changed",
    )
    .into()
}

fn availability(error: &anyhow::Error) -> MediaAvailability {
    error
        .downcast_ref::<MediaAccessError>()
        .map_or(MediaAvailability::IoUnavailable, |error| error.availability)
}

impl MediaResultStore {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }

    pub async fn initialize(&self) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for statement in [
            "CREATE TABLE IF NOT EXISTS media_output_roots_v1 (root_id TEXT PRIMARY KEY, path TEXT NOT NULL, identity TEXT NOT NULL, payload TEXT NOT NULL, UNIQUE(path, identity))",
            "CREATE TABLE IF NOT EXISTS media_results_v1 (result_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, root_id TEXT NOT NULL REFERENCES media_output_roots_v1(root_id), request_digest TEXT NOT NULL, request_payload TEXT NOT NULL, created_at INTEGER NOT NULL, state TEXT NOT NULL CHECK(state IN ('writing','prepared','ready','failed')), identity TEXT, payload TEXT)",
            "CREATE INDEX IF NOT EXISTS media_results_session_v1 ON media_results_v1(session_id,result_id)",
        ] {
            sqlx::query(statement).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn register_output_root(&self, path: &Path) -> Result<MediaOutputRoot> {
        let guard = GuardedDirectory::open(path, true)?;
        let root = MediaOutputRoot {
            root_id: uuid::Uuid::new_v4().to_string(),
            path: guard.path.clone(),
            identity: guard.identity.clone(),
        };
        let path = root.path.to_str().context("Media root is not Unicode")?;
        sqlx::query("INSERT INTO media_output_roots_v1(root_id,path,identity,payload) VALUES(?,?,?,?) ON CONFLICT(path,identity) DO NOTHING")
            .bind(&root.root_id).bind(path).bind(&root.identity).bind(serde_json::to_string(&root)?)
            .execute(&self.pool).await?;
        let payload: String = sqlx::query_scalar(
            "SELECT payload FROM media_output_roots_v1 WHERE path=? AND identity=?",
        )
        .bind(path)
        .bind(&root.identity)
        .fetch_one(&self.pool)
        .await?;
        Ok(serde_json::from_str(&payload)?)
    }

    async fn root(&self, id: &str) -> Result<MediaOutputRoot> {
        let payload: String =
            sqlx::query_scalar("SELECT payload FROM media_output_roots_v1 WHERE root_id=?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
                .context("Unknown media output root")?;
        let root: MediaOutputRoot = serde_json::from_str(&payload)?;
        ensure!(
            root.root_id == id,
            "Media output root record is inconsistent"
        );
        Ok(root)
    }

    async fn record(&self, session_id: &str, result_id: &str) -> Result<Option<Record>> {
        let row = sqlx::query("SELECT request_digest,request_payload,created_at,state,identity,payload FROM media_results_v1 WHERE session_id=? AND result_id=?")
            .bind(session_id).bind(result_id).fetch_optional(&self.pool).await?;
        row.map(|row| {
            let request: PublishMediaRequest =
                serde_json::from_str(row.try_get::<&str, _>("request_payload")?)?;
            request.validate()?;
            let request_digest: String = row.try_get("request_digest")?;
            ensure!(
                request.session_id == session_id
                    && request.result_id == result_id
                    && files::request_digest(&request)? == request_digest,
                "Media publication record is inconsistent"
            );
            let payload: Option<String> = row.try_get("payload")?;
            Ok(Record {
                request,
                request_digest,
                created_at: row.try_get("created_at")?,
                state: row.try_get("state")?,
                identity: row.try_get("identity")?,
                result: payload
                    .map(|value| serde_json::from_str(&value))
                    .transpose()?,
            })
        })
        .transpose()
    }

    pub async fn begin_publish(&self, request: PublishMediaRequest) -> Result<BeginMediaPublish> {
        request.validate()?;
        let digest = files::request_digest(&request)?;
        if let Some(record) = self.record(&request.session_id, &request.result_id).await? {
            ensure!(
                record.request_digest == digest,
                "Media result ID conflicts with another publication"
            );
            if record.state == "prepared" {
                self.recover_record(record).await?;
            }
            let content = self
                .open_content(&request.session_id, &request.result_id)
                .await?;
            return Ok(BeginMediaPublish::Existing(content.result));
        }
        let root = self.root(&request.root_id).await?;
        let root_guard = checked_root(&root)?;
        let directory = root_guard.child(&relative_directory(&request), true)?;
        let created_at = chrono::Utc::now().timestamp_millis();
        sqlx::query("INSERT INTO media_results_v1(result_id,session_id,root_id,request_digest,request_payload,created_at,state) VALUES(?,?,?,?,?,?,'writing')")
            .bind(&request.result_id).bind(&request.session_id).bind(&request.root_id).bind(&digest)
            .bind(serde_json::to_string(&request)?).bind(created_at).execute(&self.pool).await?;
        let file = directory.open_file(&stage_name(&request.result_id), true, true)?;
        let identity = files::identity(&file, false)?;
        sqlx::query("UPDATE media_results_v1 SET identity=? WHERE result_id=? AND state='writing' AND request_digest=?")
            .bind(&identity).bind(&request.result_id).bind(&digest).execute(&self.pool).await?;
        Ok(BeginMediaPublish::Staging(StagedMedia {
            request,
            created_at,
            file,
            identity,
            directory,
            _root: root_guard,
            written: 0,
        }))
    }

    pub async fn abort_staged(&self, staged: StagedMedia) -> Result<()> {
        let updated = sqlx::query("UPDATE media_results_v1 SET state='failed' WHERE result_id=? AND session_id=? AND request_digest=? AND identity=? AND state='writing'")
            .bind(&staged.request.result_id)
            .bind(&staged.request.session_id)
            .bind(files::request_digest(&staged.request)?)
            .bind(&staged.identity)
            .execute(&self.pool).await?;
        ensure!(
            updated.rows_affected() == 1,
            "Media preparation ownership changed"
        );
        files::discard_pending(&staged.file)
    }

    pub async fn publish(&self, staged: StagedMedia) -> Result<MediaResultRef> {
        let prepared = self.prepare(staged).await?;
        self.commit(prepared).await
    }

    async fn prepare(&self, mut staged: StagedMedia) -> Result<PreparedMedia> {
        let result_id = staged.request.result_id.clone();
        let (staged, checked) = tokio::task::spawn_blocking(move || {
            let checked = inspect_staged(&mut staged);
            (staged, checked)
        })
        .await?;
        let result = match checked {
            Ok(result) => result,
            Err(error) => {
                self.abort_staged(staged).await?;
                return Err(error);
            }
        };
        let updated = sqlx::query("UPDATE media_results_v1 SET state='prepared',payload=? WHERE result_id=? AND state='writing' AND identity=?")
            .bind(serde_json::to_string(&result)?).bind(&result_id).bind(&staged.identity)
            .execute(&self.pool).await?;
        ensure!(
            updated.rows_affected() == 1,
            "Media preparation ownership changed"
        );
        Ok(PreparedMedia { staged, result })
    }
}

fn inspect_staged(staged: &mut StagedMedia) -> Result<MediaResultRef> {
    staged.file.sync_all().map_err(MediaAccessError::io)?;
    let verified = formats::inspect(&mut staged.file)?;
    ensure!(
        verified.kind == staged.request.kind,
        "Decoded media kind differs from the requested task"
    );
    let bytes = staged.file.metadata()?.len();
    let sha256 = files::digest(&mut staged.file)?;
    let request = &staged.request;
    let file_name = format!("{}.{}", request.result_id, verified.extension);
    Ok(MediaResultRef {
        schema_version: 1,
        result_id: request.result_id.clone(),
        session_id: request.session_id.clone(),
        association: request.association.clone(),
        job_id: request.job_id.clone(),
        kind: verified.kind,
        mime: verified.mime.into(),
        relative_path: format!("{}/{}", relative_directory(request), file_name),
        file_name,
        bytes,
        sha256,
        created_at: staged.created_at,
        root_id: request.root_id.clone(),
        spec: verified.spec,
        ownership: ownership(&request.source),
        source: request.source.clone(),
    })
}

impl MediaResultStore {
    async fn commit(&self, prepared: PreparedMedia) -> Result<MediaResultRef> {
        files::rename(
            &prepared.staged.file,
            &prepared
                .staged
                .directory
                .path
                .join(&prepared.result.file_name),
        )?;
        self.mark_ready(&prepared.result, &prepared.staged.identity)
            .await?;
        Ok(prepared.result)
    }

    async fn mark_ready(&self, result: &MediaResultRef, identity: &str) -> Result<()> {
        let updated = sqlx::query("UPDATE media_results_v1 SET state='ready' WHERE result_id=? AND session_id=? AND state IN ('prepared','ready') AND identity=? AND payload=?")
            .bind(&result.result_id).bind(&result.session_id).bind(identity).bind(serde_json::to_string(result)?)
            .execute(&self.pool).await?;
        ensure!(
            updated.rows_affected() == 1,
            "Media publication journal no longer matches the verified file"
        );
        Ok(())
    }

    pub async fn read(
        &self,
        session_id: &str,
        result_id: &str,
    ) -> Result<Option<MediaResultStatus>> {
        let Some(record) = self.record(session_id, result_id).await? else {
            return Ok(None);
        };
        let Some(result) = record.result else {
            return Ok(None);
        };
        validate_result(&record.request, record.created_at, &result)?;
        let status = if record.state == "ready" {
            match self.open_content(session_id, result_id).await {
                Ok(_) => MediaAvailability::Available,
                Err(error) => availability(&error),
            }
        } else {
            MediaAvailability::Publishing
        };
        Ok(Some(MediaResultStatus {
            result,
            availability: status,
        }))
    }

    pub async fn open_content(
        &self,
        session_id: &str,
        result_id: &str,
    ) -> Result<VerifiedMediaContent> {
        let record = self.record(session_id, result_id).await?.ok_or_else(|| {
            MediaAccessError::new(
                MediaAvailability::Missing,
                "No media result exists in this session",
            )
        })?;
        ensure!(
            record.state == "ready",
            "Media result has not completed publication"
        );
        let root = self.root(&record.request.root_id).await?;
        tokio::task::spawn_blocking(move || open_record(record, root)).await?
    }

    /// Finishes verified publications. `Publishing` for an unvalidated writing
    /// record needs task-owner reconciliation; it does not prove a live producer.
    /// Registered results are never republished or retargeted by recovery.
    pub async fn recover(&self) -> Result<Vec<MediaRecoveryOutcome>> {
        let rows = sqlx::query("SELECT session_id,result_id,state FROM media_results_v1 WHERE state IN ('prepared','writing') ORDER BY result_id")
            .fetch_all(&self.pool).await?;
        let mut outcomes = Vec::new();
        for row in rows {
            let result_id: String = match row.try_get("result_id") {
                Ok(value) => value,
                Err(_) => {
                    tracing::warn!("Media recovery skipped an unreadable journal identity");
                    continue;
                }
            };
            let recovered: Result<MediaAvailability> = async {
                let session_id: String = row.try_get("session_id")?;
                let state: String = row.try_get("state")?;
                if state == "writing" {
                    // Unvalidated bytes are never promoted during recovery. A task
                    // retry uses a new result ID instead of overwriting this output.
                    return Ok(MediaAvailability::Publishing);
                }
                let record = self
                    .record(&session_id, &result_id)
                    .await?
                    .context("Publication journal disappeared")?;
                self.recover_record(record).await?;
                Ok(MediaAvailability::Available)
            }
            .await;
            outcomes.push(MediaRecoveryOutcome {
                result_id,
                availability: recovered.unwrap_or_else(|error| availability(&error)),
            });
        }
        Ok(outcomes)
    }

    async fn recover_record(&self, record: Record) -> Result<MediaResultRef> {
        let root = self.root(&record.request.root_id).await?;
        let (result, identity, _file, _directory, _root) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                let result = record
                    .result
                    .context("Prepared media has no verified metadata")?;
                validate_result(&record.request, record.created_at, &result)?;
                let identity = record
                    .identity
                    .context("Prepared media has no file identity")?;
                let root = checked_root(&root)?;
                let directory = root.child(&relative_directory(&record.request), false)?;
                let mut file = match directory.open_file(&result.file_name, false, false) {
                    Ok(mut file) => {
                        verify_content(&mut file, &identity, &result)?;
                        file
                    }
                    Err(error) if availability(&error) == MediaAvailability::Missing => {
                        let mut file =
                            directory.open_file(&stage_name(&result.result_id), true, false)?;
                        verify_content(&mut file, &identity, &result)?;
                        files::rename(&file, &directory.path.join(&result.file_name))?;
                        file
                    }
                    Err(error) => return Err(error),
                };
                file.seek(SeekFrom::Start(0))?;
                Ok((result, identity, file, directory, root))
            })
            .await??;
        self.mark_ready(&result, &identity).await?;
        Ok(result)
    }
}

fn checked_root(root: &MediaOutputRoot) -> Result<GuardedDirectory> {
    let guard = GuardedDirectory::open(&root.path, false)?;
    if guard.identity != root.identity {
        return Err(MediaAccessError::new(
            MediaAvailability::UnsafePath,
            "Media output root has been replaced",
        )
        .into());
    }
    Ok(guard)
}

fn validate_result(
    request: &PublishMediaRequest,
    created_at: i64,
    result: &MediaResultRef,
) -> Result<()> {
    domain::validate_digest(&result.sha256)?;
    let extension = match result.mime.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "audio/wav" => "wav",
        "video/x-msvideo" => "avi",
        _ => return Err(tampered()),
    };
    if result.schema_version != 1
        || result.result_id != request.result_id
        || result.session_id != request.session_id
        || result.job_id != request.job_id
        || result.root_id != request.root_id
        || result.association != request.association
        || result.source != request.source
        || result.ownership != ownership(&request.source)
        || result.kind != request.kind
        || result.created_at != created_at
        || result.bytes == 0
        || result.bytes > MAX_MEDIA_BYTES
        || result.file_name != format!("{}.{}", request.result_id, extension)
        || result.relative_path != format!("{}/{}", relative_directory(request), result.file_name)
    {
        return Err(tampered());
    }
    Ok(())
}

fn verify_content(file: &mut File, expected_identity: &str, result: &MediaResultRef) -> Result<()> {
    if files::identity(file, false)? != expected_identity
        || file.metadata()?.len() != result.bytes
        || files::digest(file)? != result.sha256
    {
        return Err(tampered());
    }
    let decoded = formats::inspect(file).map_err(|_| tampered())?;
    if decoded.kind != result.kind || decoded.mime != result.mime || decoded.spec != result.spec {
        return Err(tampered());
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(())
}

fn open_record(record: Record, root: MediaOutputRoot) -> Result<VerifiedMediaContent> {
    let result = record
        .result
        .context("Ready media has no result metadata")?;
    validate_result(&record.request, record.created_at, &result)?;
    let identity = record
        .identity
        .context("Ready media has no file identity")?;
    let root = checked_root(&root)?;
    let directory = root.child(&relative_directory(&record.request), false)?;
    let mut file = directory.open_file(&result.file_name, false, false)?;
    verify_content(&mut file, &identity, &result)?;
    Ok(VerifiedMediaContent {
        path: directory.path.join(&result.file_name),
        result,
        file,
        _directory: directory,
        _root: root,
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
