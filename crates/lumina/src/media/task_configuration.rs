use anyhow::{bail, ensure, Context, Result};
use lumina_providers::local_inference::lifecycle::{
    AudioTaskProfile, AudioTaskProtocol, GenerationParameterOrigin, GenerationParameterService,
    GenerationParameterSnapshot, GenerationProfile, GenerationTaskRequest,
    ModelTaskParameterAdapter, PocketTtsParameterService, PocketTtsPreparedTask, PocketTtsSnapshot,
    PreparedGenerationTask, Qwen3AsrParameterService, Qwen3AsrPreparedTask, Qwen3AsrSnapshot,
    RuntimeBuild, Sha256Digest, TaskParameterIdentity,
};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{Pool, Row, Sqlite};

use super::domain::{validate_digest, validate_id};
use super::{MediaResultRef, MediaSource, PublishMediaRequest};

pub const MAX_TASK_CONFIGURATION_BYTES: usize = 256 * 1024;
const MAX_IDENTITY_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum TaskConfigurationSnapshot {
    Generation(Box<GenerationParameterSnapshot>),
    PocketTts(Box<PocketTtsSnapshot>),
    Qwen3Asr(Box<Qwen3AsrSnapshot>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskConfigurationRecord {
    session_id: String,
    job_id: String,
    identity: TaskParameterIdentity,
    snapshot_json: String,
    snapshot_sha256: String,
    snapshot: TaskConfigurationSnapshot,
}

impl TaskConfigurationRecord {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    pub fn identity(&self) -> &TaskParameterIdentity {
        &self.identity
    }

    pub fn snapshot(&self) -> &TaskConfigurationSnapshot {
        &self.snapshot
    }

    pub fn snapshot_json(&self) -> &str {
        &self.snapshot_json
    }

    pub fn generation_config_sha256(&self) -> &str {
        &self.snapshot_sha256
    }

    fn runtime(&self) -> &RuntimeBuild {
        match &self.snapshot {
            TaskConfigurationSnapshot::Generation(value) => &value.binding.runtime,
            TaskConfigurationSnapshot::PocketTts(value) => &value.metadata.binding.runtime,
            TaskConfigurationSnapshot::Qwen3Asr(value) => &value.metadata.binding.runtime,
        }
    }

    fn verify_source(&self, session_id: &str, job_id: &str, source: &MediaSource) -> Result<()> {
        ensure!(
            session_id == self.session_id && job_id == self.job_id,
            "Media output belongs to a different task configuration"
        );
        let MediaSource::LocalModel {
            installation_id,
            version_id,
            runtime,
            generation_config_sha256,
            ..
        } = source
        else {
            bail!("MCP media has no local generation task configuration");
        };
        ensure!(
            installation_id == &self.identity.installation_id
                && version_id == &self.identity.version_id
                && generation_config_sha256 == &self.snapshot_sha256
                && runtime.adapter_id == self.runtime().adapter_id
                && runtime.engine_revision == self.runtime().engine_revision
                && runtime.binary_sha256 == self.runtime().binary_sha256.as_str(),
            "Media provenance differs from its persisted task configuration"
        );
        Ok(())
    }
}

/// Shares the initialized session pool. Saving configuration never grants execution
/// permission, publishes a result, or changes an existing task's scheduling state.
#[derive(Clone)]
pub struct TaskConfigurationStore {
    pool: Pool<Sqlite>,
}

impl TaskConfigurationStore {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }

    pub async fn initialize(&self) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&mut *tx)
            .await?;
        ensure!(
            foreign_keys == 1,
            "Task configurations require SQLite foreign keys"
        );
        let sessions: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='sessions'",
        )
        .fetch_one(&mut *tx)
        .await?;
        ensure!(
            sessions == 1,
            "Initialize session storage before task configurations"
        );
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS media_task_configurations_v1 (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE CHECK(length(CAST(session_id AS BLOB)) BETWEEN 1 AND 96),
                job_id TEXT NOT NULL CHECK(length(CAST(job_id AS BLOB)) BETWEEN 1 AND 96),
                parameter_schema_id TEXT NOT NULL CHECK(length(CAST(parameter_schema_id AS BLOB)) BETWEEN 1 AND 128),
                identity_json TEXT NOT NULL CHECK(length(CAST(identity_json AS BLOB)) BETWEEN 1 AND 16384),
                snapshot_json TEXT NOT NULL CHECK(length(CAST(snapshot_json AS BLOB)) BETWEEN 1 AND 262144),
                snapshot_sha256 TEXT NOT NULL CHECK(length(snapshot_sha256)=64),
                PRIMARY KEY(session_id,job_id)
            )",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS media_task_transcripts_v1 (
                session_id TEXT NOT NULL,
                job_id TEXT NOT NULL,
                association_json TEXT NOT NULL CHECK(length(CAST(association_json AS BLOB)) BETWEEN 1 AND 8192),
                snapshot_sha256 TEXT NOT NULL CHECK(length(snapshot_sha256)=64),
                content_sha256 TEXT NOT NULL CHECK(length(content_sha256)=64),
                text TEXT NOT NULL CHECK(length(CAST(text AS BLOB)) <= 4194304),
                PRIMARY KEY(session_id,job_id),
                FOREIGN KEY(session_id,job_id) REFERENCES media_task_configurations_v1(session_id,job_id) ON DELETE CASCADE
            )",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub(super) async fn save_transcript(
        &self,
        transcript: &super::NativeTaskTranscript,
    ) -> Result<()> {
        sqlx::query("INSERT INTO media_task_transcripts_v1(session_id,job_id,association_json,snapshot_sha256,content_sha256,text) VALUES(?,?,?,?,?,?) ON CONFLICT(session_id,job_id) DO NOTHING")
            .bind(&transcript.session_id)
            .bind(&transcript.job_id)
            .bind(serde_json::to_string(&transcript.association)?)
            .bind(&transcript.generation_config_sha256)
            .bind(&transcript.sha256)
            .bind(&transcript.text)
            .execute(&self.pool).await?;
        let stored = self
            .read_transcript(&transcript.session_id, &transcript.job_id)
            .await?;
        ensure!(
            stored.as_ref() == Some(transcript),
            "Transcript conflicts with the stored task result"
        );
        Ok(())
    }

    pub async fn read_transcript(
        &self,
        session_id: &str,
        job_id: &str,
    ) -> Result<Option<super::NativeTaskTranscript>> {
        validate_id(session_id)?;
        validate_id(job_id)?;
        let row = sqlx::query(
            "SELECT association_json,snapshot_sha256,content_sha256,
                CASE WHEN length(CAST(text AS BLOB))<=4194304 THEN text END AS text
             FROM media_task_transcripts_v1 WHERE session_id=? AND job_id=?",
        )
        .bind(session_id)
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let config = self
            .read(session_id, job_id)
            .await?
            .context("Missing transcript task configuration")?;
        let snapshot_sha256: String = row.try_get("snapshot_sha256")?;
        ensure!(
            matches!(config.snapshot(), TaskConfigurationSnapshot::Qwen3Asr(_))
                && config.generation_config_sha256() == snapshot_sha256,
            "Transcript configuration mismatch"
        );
        let text: String = row
            .try_get::<Option<String>, _>("text")?
            .context("Stored transcript exceeds its byte limit")?;
        let sha256: String = row.try_get("content_sha256")?;
        ensure!(
            Sha256Digest::of_bytes(text.as_bytes()).as_str() == sha256,
            "Stored transcript content mismatch"
        );
        Ok(Some(super::NativeTaskTranscript {
            session_id: session_id.into(),
            job_id: job_id.into(),
            association: serde_json::from_str(row.try_get("association_json")?)?,
            generation_config_sha256: snapshot_sha256,
            bytes: text.len() as u64,
            sha256,
            text,
        }))
    }

    pub async fn save_generation(
        &self,
        task: &PreparedGenerationTask,
    ) -> Result<TaskConfigurationRecord> {
        let snapshot = task.snapshot();
        self.save(parse_snapshot(
            snapshot.profile.parameter_schema_id(),
            &bounded_json(snapshot)?,
        )?)
        .await
    }

    pub async fn save_pocket_tts(
        &self,
        task: &PocketTtsPreparedTask,
    ) -> Result<TaskConfigurationRecord> {
        let snapshot = task.snapshot();
        self.save(parse_snapshot(
            &snapshot.metadata.identity.parameter_schema_id,
            &bounded_json(snapshot)?,
        )?)
        .await
    }

    pub async fn save_qwen3_asr(
        &self,
        task: &Qwen3AsrPreparedTask,
    ) -> Result<TaskConfigurationRecord> {
        let snapshot = task.snapshot();
        self.save(parse_snapshot(
            &snapshot.metadata.identity.parameter_schema_id,
            &bounded_json(snapshot)?,
        )?)
        .await
    }

    async fn save(&self, record: TaskConfigurationRecord) -> Result<TaskConfigurationRecord> {
        self.validate_audio_parent(&record).await?;
        let identity_json = serde_json::to_string(&record.identity)?;
        ensure!(
            identity_json.len() <= MAX_IDENTITY_BYTES,
            "Task identity exceeds its byte limit"
        );
        sqlx::query("INSERT INTO media_task_configurations_v1(session_id,job_id,parameter_schema_id,identity_json,snapshot_json,snapshot_sha256) VALUES(?,?,?,?,?,?) ON CONFLICT(session_id,job_id) DO NOTHING")
            .bind(&record.session_id)
            .bind(&record.job_id)
            .bind(&record.identity.parameter_schema_id)
            .bind(identity_json)
            .bind(&record.snapshot_json)
            .bind(&record.snapshot_sha256)
            .execute(&self.pool)
            .await?;
        let stored = self
            .read(&record.session_id, &record.job_id)
            .await?
            .context("Task configuration disappeared during persistence")?;
        ensure!(
            stored == record,
            "Task job ID conflicts with a different immutable configuration"
        );
        Ok(stored)
    }

    async fn validate_audio_parent(&self, child: &TaskConfigurationRecord) -> Result<()> {
        let parent = match child.snapshot() {
            TaskConfigurationSnapshot::PocketTts(s) => &s.metadata.derivation,
            TaskConfigurationSnapshot::Qwen3Asr(s) => &s.metadata.derivation,
            TaskConfigurationSnapshot::Generation(_) => return Ok(()),
        };
        let Some(parent) = parent else { return Ok(()) };
        let parent = self
            .read(child.session_id(), &parent.parent_job_id)
            .await?
            .context("Missing parent audio task in this session")?;
        match (child.snapshot(), parent.snapshot()) {
            (TaskConfigurationSnapshot::PocketTts(c), TaskConfigurationSnapshot::PocketTts(p)) => {
                PocketTtsParameterService::new(c.metadata.binding.clone())?.validate_parent(c, p)?
            }
            (TaskConfigurationSnapshot::Qwen3Asr(c), TaskConfigurationSnapshot::Qwen3Asr(p)) => {
                Qwen3AsrParameterService::new(c.metadata.binding.clone())?.validate_parent(c, p)?
            }
            _ => bail!("Parent task uses a different audio adapter"),
        }
        Ok(())
    }

    pub async fn read(
        &self,
        session_id: &str,
        job_id: &str,
    ) -> Result<Option<TaskConfigurationRecord>> {
        validate_id(session_id)?;
        validate_id(job_id)?;
        let row = sqlx::query(
            "SELECT
                CASE WHEN length(CAST(parameter_schema_id AS BLOB)) BETWEEN 1 AND 128 THEN parameter_schema_id END AS parameter_schema_id,
                CASE WHEN length(CAST(snapshot_sha256 AS BLOB))=64 THEN snapshot_sha256 END AS snapshot_sha256,
                CASE WHEN length(CAST(identity_json AS BLOB)) BETWEEN 1 AND 16384 THEN identity_json END AS identity_json,
                CASE WHEN length(CAST(snapshot_json AS BLOB)) BETWEEN 1 AND 262144 THEN snapshot_json END AS snapshot_json
             FROM media_task_configurations_v1 WHERE session_id=? AND job_id=?",
        )
        .bind(session_id)
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let schema: Option<String> = row.try_get("parameter_schema_id")?;
            let payload: Option<String> = row.try_get("snapshot_json")?;
            let identity_json: Option<String> = row.try_get("identity_json")?;
            let sha: Option<String> = row.try_get("snapshot_sha256")?;
            let schema = schema.context("Stored task schema identifier exceeds its byte limit")?;
            let sha = sha.context("Stored task digest has an invalid byte length")?;
            validate_digest(&sha)?;
            let payload = payload.context("Stored configuration exceeds its byte limit")?;
            let identity_json =
                identity_json.context("Stored task identity exceeds its byte limit")?;
            let identity: TaskParameterIdentity = serde_json::from_str(&identity_json)?;
            let record = parse_snapshot(&schema, &payload)?;
            ensure!(
                record.session_id == session_id
                    && record.job_id == job_id
                    && record.identity == identity
                    && record.snapshot_sha256 == sha
                    && serde_json::to_string(&identity)? == identity_json,
                "Stored task configuration identity or digest is inconsistent"
            );
            Ok(record)
        })
        .transpose()
    }

    pub async fn list(
        &self,
        session_id: &str,
        after_job_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<TaskConfigurationRecord>> {
        validate_id(session_id)?;
        if let Some(value) = after_job_id {
            validate_id(value)?;
        }
        ensure!(
            (1..=100).contains(&limit),
            "Task configuration page limit must be 1..100"
        );
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT job_id FROM media_task_configurations_v1 WHERE session_id=? AND (? IS NULL OR job_id>?) ORDER BY job_id LIMIT ?",
        )
        .bind(session_id).bind(after_job_id).bind(after_job_id).bind(limit)
        .fetch_all(&self.pool).await?;
        let mut records = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(record) = self.read(session_id, &id).await? {
                records.push(record);
            }
        }
        Ok(records)
    }

    pub async fn verify_publish(
        &self,
        request: &PublishMediaRequest,
    ) -> Result<TaskConfigurationRecord> {
        request.validate()?;
        let record = self
            .read(&request.session_id, &request.job_id)
            .await?
            .context("Local model output has no persisted task configuration")?;
        record.verify_source(&request.session_id, &request.job_id, &request.source)?;
        Ok(record)
    }

    pub async fn verify_result(&self, result: &MediaResultRef) -> Result<TaskConfigurationRecord> {
        let record = self
            .read(&result.session_id, &result.job_id)
            .await?
            .context("Local model result has no persisted task configuration")?;
        record.verify_source(&result.session_id, &result.job_id, &result.source)?;
        Ok(record)
    }
}

fn bounded_json(value: &impl Serialize) -> Result<String> {
    struct Writer(Vec<u8>);
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_TASK_CONFIGURATION_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Task configuration exceeds its byte limit",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer(Vec::new());
    serde_json::to_writer(&mut writer, value)?;
    Ok(String::from_utf8(writer.0)?)
}

fn canonical_generation(
    service: &GenerationParameterService,
    session_id: &str,
    job_id: &str,
    mut parameters: Value,
) -> Result<PreparedGenerationTask> {
    let fields = parameters
        .get_mut("parameters")
        .and_then(Value::as_object_mut)
        .context("Missing effective generation parameters")?;
    fields.remove("duration");
    let seed = fields
        .get("seed")
        .context("Missing effective generation seed")?
        .clone();
    fields.insert("seed".into(), json!({"kind":"value","value":seed}));
    Ok(service.compile(GenerationTaskRequest {
        session_id: session_id.into(),
        job_id: job_id.into(),
        parameters: serde_json::from_value(parameters)?,
    })?)
}

fn validate_generation(snapshot: &GenerationParameterSnapshot) -> Result<TaskParameterIdentity> {
    let service = GenerationParameterService::new(snapshot.profile, snapshot.binding.clone())?;
    let identity = service.identity()?;
    let effective = serde_json::to_value(&snapshot.parameters)?;
    let canonical = canonical_generation(
        &service,
        &snapshot.session_id,
        &snapshot.job_id,
        effective.clone(),
    )?;
    let fresh = canonical.snapshot();
    ensure!(
        snapshot.schema_version == fresh.schema_version
            && service.matches_descriptor_digest(&snapshot.descriptor_digest)?
            && snapshot.execution_status == fresh.execution_status
            && snapshot.parameters == fresh.parameters
            && snapshot.engine == fresh.engine
            && snapshot.observation == fresh.observation,
        "Stored generation configuration differs from its bound adapter"
    );
    let fields = effective["parameters"]
        .as_object()
        .context("Missing generation fields")?;
    ensure!(
        fields.len() == snapshot.origins.len()
            && fields.keys().all(|key| snapshot.origins.contains_key(key)),
        "Generation parameter origins do not cover the effective configuration"
    );
    if let Some(parent) = &snapshot.parent {
        ensure!(
            uuid::Uuid::parse_str(&parent.result_id)?.to_string() == parent.result_id,
            "Invalid parent media result ID"
        );
    } else {
        ensure!(
            snapshot.changes.is_empty(),
            "Unparented generation has parameter changes"
        );
    }
    for origin in snapshot.origins.values() {
        match origin {
            GenerationParameterOrigin::Parent { result_id } => ensure!(
                snapshot
                    .parent
                    .as_ref()
                    .is_some_and(|parent| &parent.result_id == result_id),
                "Parameter origin refers to a different parent result"
            ),
            GenerationParameterOrigin::RecordedDefault { evidence } => ensure!(
                evidence == &service.descriptor().evidence,
                "Parameter origin refers to another model's defaults"
            ),
            _ => {}
        }
    }
    if !snapshot.changes.is_empty() {
        let mut before = effective.clone();
        for (key, change) in &snapshot.changes {
            ensure!(
                fields.get(key) == Some(&change.after),
                "Changed parameter differs from effective configuration"
            );
            before["parameters"][key] = change.before.clone();
        }
        let previous = canonical_generation(
            &service,
            &snapshot.session_id,
            &snapshot.job_id,
            before.clone(),
        )?;
        ensure!(
            serde_json::to_value(&previous.snapshot().parameters)? == before,
            "Previous generation parameters do not match their schema"
        );
    }
    Ok(identity)
}

fn parse_snapshot(schema: &str, payload: &str) -> Result<TaskConfigurationRecord> {
    ensure!(
        !payload.is_empty() && payload.len() <= MAX_TASK_CONFIGURATION_BYTES,
        "Task configuration exceeds its byte limit"
    );
    let audio_profile = AudioTaskProfile::for_parameter_schema(schema);
    let (session_id, job_id, identity, canonical, snapshot) = match audio_profile
        .map(|profile| (profile.protocol(), profile))
    {
        None if GenerationProfile::registered()
            .any(|profile| profile.parameter_schema_id() == schema) =>
        {
            let value: GenerationParameterSnapshot = serde_json::from_str(payload)?;
            let identity = validate_generation(&value)?;
            (
                value.session_id.clone(),
                value.job_id.clone(),
                identity,
                bounded_json(&value)?,
                TaskConfigurationSnapshot::Generation(Box::new(value)),
            )
        }
        Some((AudioTaskProtocol::PocketTtsOffline, profile)) => {
            let value: PocketTtsSnapshot = serde_json::from_str(payload)?;
            let service =
                PocketTtsParameterService::for_profile(profile, value.metadata.binding.clone())?;
            service.validate_snapshot(&value)?;
            (
                value.metadata.session_id.clone(),
                value.metadata.job_id.clone(),
                service.identity()?,
                bounded_json(&value)?,
                TaskConfigurationSnapshot::PocketTts(Box::new(value)),
            )
        }
        Some((AudioTaskProtocol::Qwen3AsrOffline, profile)) => {
            let value: Qwen3AsrSnapshot = serde_json::from_str(payload)?;
            let service =
                Qwen3AsrParameterService::for_profile(profile, value.metadata.binding.clone())?;
            service.validate_snapshot(&value)?;
            (
                value.metadata.session_id.clone(),
                value.metadata.job_id.clone(),
                service.identity()?,
                bounded_json(&value)?,
                TaskConfigurationSnapshot::Qwen3Asr(Box::new(value)),
            )
        }
        None => bail!("Unsupported persisted task parameter schema: {schema}"),
    };
    validate_id(&session_id)?;
    validate_id(&job_id)?;
    ensure!(
        identity.parameter_schema_id == schema,
        "Stored parameter schema differs from model identity"
    );
    ensure!(
        canonical == payload,
        "Stored task JSON is noncanonical or contains unrecognized fields"
    );
    let snapshot_sha256 = Sha256Digest::of_bytes(canonical.as_bytes())
        .as_str()
        .to_owned();
    Ok(TaskConfigurationRecord {
        session_id,
        job_id,
        identity,
        snapshot_json: canonical,
        snapshot_sha256,
        snapshot,
    })
}

#[cfg(test)]
#[path = "task_configuration_tests.rs"]
mod tests;
