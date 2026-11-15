use anyhow::{bail, ensure, Context, Result};
use lumina_providers::local_inference::lifecycle::{
    AudioTaskProfile, AudioTaskProtocol, GenerationParameterOrigin, GenerationParameterService,
    GenerationParameterSnapshot, GenerationProfile, GenerationTaskRequest,
    ModelTaskParameterAdapter, NativeTaskPlan, PocketTtsParameterService, PocketTtsPreparedTask,
    PocketTtsSnapshot, PreparedGenerationTask, ProjectTaskCompilationScope,
    Qwen3AsrParameterService, Qwen3AsrPreparedTask, Qwen3AsrSnapshot, RuntimeBuild, Sha256Digest,
    StableAudioMusicParameterService, StableAudioMusicPreparedTask, StableAudioMusicSnapshot,
    TaskParameterIdentity,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Pool, Row, Sqlite, SqliteConnection, Transaction};

use super::domain::{validate_digest, validate_id};
use super::project_task_reuse::{
    PreparedProjectTask, ProjectCompilationBinding, VerifiedProjectTaskAuthorization,
};
use super::{MediaResultRef, MediaSource, PublishMediaRequest};

pub const MAX_TASK_CONFIGURATION_BYTES: usize = 256 * 1024;
const MAX_IDENTITY_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum TaskConfigurationSnapshot {
    Generation(Box<GenerationParameterSnapshot>),
    PocketTts(Box<PocketTtsSnapshot>),
    Qwen3Asr(Box<Qwen3AsrSnapshot>),
    StableAudioMusic(Box<StableAudioMusicSnapshot>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskConfigurationRecord {
    session_id: String,
    job_id: String,
    identity: TaskParameterIdentity,
    snapshot_json: String,
    snapshot_sha256: String,
    snapshot: TaskConfigurationSnapshot,
    project_scope: Option<ProjectCompilationBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ArchivedTaskConfiguration {
    parameter_schema_id: String,
    identity_json: String,
    snapshot_json: String,
    snapshot_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_scope: Option<ProjectCompilationBinding>,
}

impl ArchivedTaskConfiguration {
    pub(super) fn from_record(record: &TaskConfigurationRecord) -> Result<Self> {
        Ok(Self {
            parameter_schema_id: record.identity.parameter_schema_id.clone(),
            identity_json: serde_json::to_string(&record.identity)?,
            snapshot_json: record.snapshot_json.clone(),
            snapshot_sha256: record.snapshot_sha256.clone(),
            project_scope: record.project_scope.clone(),
        })
    }

    pub(super) fn validate(&self, session: &str, job: &str) -> Result<TaskConfigurationRecord> {
        ensure!(
            self.identity_json.len() <= MAX_IDENTITY_BYTES,
            "Archived task identity exceeds its byte limit"
        );
        let record = parse_snapshot_with_project_scope(
            &self.parameter_schema_id,
            &self.snapshot_json,
            self.project_scope.as_ref(),
        )?;
        ensure!(
            record.session_id == session
                && record.job_id == job
                && record.snapshot_sha256 == self.snapshot_sha256
                && serde_json::to_string(&record.identity)? == self.identity_json,
            "Archived task configuration identity or digest is inconsistent"
        );
        Ok(record)
    }

    pub(super) async fn read(
        tx: &mut Transaction<'_, Sqlite>,
        session: &str,
        job: &str,
    ) -> Result<Self> {
        let payload: String = sqlx::query_scalar(
            "SELECT CASE WHEN length(CAST(payload AS BLOB))<=1048576 THEN payload END
             FROM project_task_configurations_v1 WHERE session_id=? AND job_id=?",
        )
        .bind(session)
        .bind(job)
        .fetch_optional(&mut **tx)
        .await?
        .context("Project result has no archived task configuration")?;
        let archive: Self = serde_json::from_str(&payload)?;
        archive.validate(session, job)?;
        Ok(archive)
    }
}

impl TaskConfigurationRecord {
    pub(super) fn from_project_plan(
        plan: &NativeTaskPlan,
        scope: &ProjectCompilationBinding,
    ) -> Result<Self> {
        let payload = match plan {
            NativeTaskPlan::Generation(task) => bounded_json(task.snapshot())?,
            NativeTaskPlan::PocketTts(task) => bounded_json(task.snapshot())?,
            NativeTaskPlan::Qwen3Asr(task) => bounded_json(task.snapshot())?,
            NativeTaskPlan::StableAudioMusic(task) => bounded_json(task.snapshot())?,
        };
        let schema = match plan {
            NativeTaskPlan::Generation(task) => task.snapshot().profile.parameter_schema_id(),
            NativeTaskPlan::PocketTts(task) => {
                &task.snapshot().metadata.identity.parameter_schema_id
            }
            NativeTaskPlan::Qwen3Asr(task) => {
                &task.snapshot().metadata.identity.parameter_schema_id
            }
            NativeTaskPlan::StableAudioMusic(task) => {
                &task.snapshot().metadata.identity.parameter_schema_id
            }
        };
        parse_snapshot_with_project_scope(schema, &payload, Some(scope))
    }

    pub(super) fn project_scope(&self) -> Option<&ProjectCompilationBinding> {
        self.project_scope.as_ref()
    }

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
            TaskConfigurationSnapshot::StableAudioMusic(value) => &value.metadata.binding.runtime,
        }
    }

    pub(super) fn verify_source(
        &self,
        session_id: &str,
        job_id: &str,
        source: &MediaSource,
    ) -> Result<()> {
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
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS project_task_configurations_v1 (
                session_id TEXT NOT NULL, job_id TEXT NOT NULL,
                payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) BETWEEN 1 AND 1048576),
                PRIMARY KEY(session_id,job_id))",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS project_task_authorizations_v1 (
            session_id TEXT NOT NULL, job_id TEXT NOT NULL,
            payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) BETWEEN 1 AND 1048576),
            sha256 TEXT NOT NULL CHECK(length(sha256)=64), PRIMARY KEY(session_id,job_id))",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        crate::project::ProjectAssetStore::new(self.pool.clone())
            .initialize()
            .await?;
        Ok(())
    }

    pub(super) async fn save_transcript(
        &self,
        transcript: &super::NativeTaskTranscript,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO media_task_transcripts_v1(session_id,job_id,association_json,snapshot_sha256,content_sha256,text) VALUES(?,?,?,?,?,?) ON CONFLICT(session_id,job_id) DO NOTHING")
            .bind(&transcript.session_id)
            .bind(&transcript.job_id)
            .bind(serde_json::to_string(&transcript.association)?)
            .bind(&transcript.generation_config_sha256)
            .bind(&transcript.sha256)
            .bind(&transcript.text)
            .execute(&mut *tx).await?;
        let stored =
            read_transcript_from(&mut tx, &transcript.session_id, &transcript.job_id).await?;
        ensure!(
            stored.as_ref() == Some(transcript),
            "Transcript conflicts with the stored task result"
        );
        super::project_assets::record_transcript(&mut tx, transcript).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn read_transcript(
        &self,
        session_id: &str,
        job_id: &str,
    ) -> Result<Option<super::NativeTaskTranscript>> {
        let mut connection = self.pool.acquire().await?;
        read_transcript_from(&mut connection, session_id, job_id).await
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

    pub async fn save_music(
        &self,
        task: &StableAudioMusicPreparedTask,
    ) -> Result<TaskConfigurationRecord> {
        let snapshot = task.snapshot();
        self.save(parse_snapshot(
            &snapshot.metadata.identity.parameter_schema_id,
            &bounded_json(snapshot)?,
        )?)
        .await
    }

    pub(crate) async fn save_project_task(
        &self,
        prepared: &PreparedProjectTask,
    ) -> Result<TaskConfigurationRecord> {
        let authorization = prepared.authorization();
        let plan = prepared.plan();
        let record = TaskConfigurationRecord::from_project_plan(plan, authorization.compilation())?;
        authorization.validate_child(&record)?;
        super::project_task_reuse::verify_final_inputs(
            &super::MediaResultStore::new(self.pool.clone()),
            &plan.input_references(),
        )
        .await?;
        let authorization_payload = authorization.encode()?;
        let identity_json = serde_json::to_string(&record.identity)?;
        ensure!(
            identity_json.len() <= MAX_IDENTITY_BYTES,
            "Task identity exceeds its byte limit"
        );
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        authorization.validate_in_tx(&mut tx).await?;
        sqlx::query("INSERT INTO project_task_authorizations_v1(session_id,job_id,payload,sha256) VALUES(?,?,?,?) ON CONFLICT(session_id,job_id) DO NOTHING")
            .bind(record.session_id()).bind(record.job_id()).bind(&authorization_payload)
            .bind(Sha256Digest::of_bytes(authorization_payload.as_bytes()).as_str())
            .execute(&mut *tx).await?;
        let stored_authorization = VerifiedProjectTaskAuthorization::read_from(
            &mut tx,
            record.session_id(),
            record.job_id(),
        )
        .await?;
        ensure!(
            stored_authorization.as_ref() == Some(authorization),
            "Project job conflicts with an immutable authorization"
        );
        let stored = save_configuration_in_tx(&mut tx, &record, identity_json).await?;
        tx.commit().await?;
        Ok(stored)
    }

    async fn save(&self, record: TaskConfigurationRecord) -> Result<TaskConfigurationRecord> {
        self.validate_audio_parent(&record).await?;
        let identity_json = serde_json::to_string(&record.identity)?;
        ensure!(
            identity_json.len() <= MAX_IDENTITY_BYTES,
            "Task identity exceeds its byte limit"
        );
        let mut tx = self.pool.begin().await?;
        let stored = save_configuration_in_tx(&mut tx, &record, identity_json).await?;
        tx.commit().await?;
        Ok(stored)
    }

    async fn validate_audio_parent(&self, child: &TaskConfigurationRecord) -> Result<()> {
        let parent = match child.snapshot() {
            TaskConfigurationSnapshot::PocketTts(s) => &s.metadata.derivation,
            TaskConfigurationSnapshot::Qwen3Asr(s) => &s.metadata.derivation,
            TaskConfigurationSnapshot::StableAudioMusic(s) => &s.metadata.derivation,
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
            (
                TaskConfigurationSnapshot::StableAudioMusic(c),
                TaskConfigurationSnapshot::StableAudioMusic(p),
            ) => StableAudioMusicParameterService::new(c.metadata.binding.clone())?
                .validate_parent(c, p)?,
            _ => bail!("Parent task uses a different audio adapter"),
        }
        Ok(())
    }

    pub async fn read(
        &self,
        session_id: &str,
        job_id: &str,
    ) -> Result<Option<TaskConfigurationRecord>> {
        let mut connection = self.pool.acquire().await?;
        read_configuration(&mut connection, session_id, job_id).await
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

async fn save_configuration_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    record: &TaskConfigurationRecord,
    identity_json: String,
) -> Result<TaskConfigurationRecord> {
    sqlx::query("INSERT INTO media_task_configurations_v1(session_id,job_id,parameter_schema_id,identity_json,snapshot_json,snapshot_sha256) VALUES(?,?,?,?,?,?) ON CONFLICT(session_id,job_id) DO NOTHING")
            .bind(&record.session_id)
            .bind(&record.job_id)
            .bind(&record.identity.parameter_schema_id)
            .bind(identity_json)
            .bind(&record.snapshot_json)
            .bind(&record.snapshot_sha256)
            .execute(&mut **tx)
            .await?;
    let stored = read_configuration(tx, &record.session_id, &record.job_id)
        .await?
        .context("Task configuration disappeared during persistence")?;
    ensure!(
        &stored == record,
        "Task job ID conflicts with a different immutable configuration"
    );
    if crate::project::ProjectAssetStore::has_project_scope_in_tx(
        tx,
        &record.session_id,
        &record.job_id,
        None,
    )
    .await?
    {
        let archive_limit = record
            .project_scope()
            .and_then(ProjectCompilationBinding::payload_tier)
            .map_or(1024 * 1024, |tier| tier.record_limit());
        let payload = super::project_payload::bounded_json(
            &ArchivedTaskConfiguration::from_record(record)?,
            archive_limit,
        )
        .context("Project task archive exceeds its byte limit or is invalid")?;
        sqlx::query("INSERT INTO project_task_configurations_v1(session_id,job_id,payload) VALUES(?,?,?) ON CONFLICT(session_id,job_id) DO NOTHING")
                .bind(&record.session_id).bind(&record.job_id).bind(&payload)
                .execute(&mut **tx).await?;
        let existing: String = sqlx::query_scalar(
            "SELECT payload FROM project_task_configurations_v1 WHERE session_id=? AND job_id=?",
        )
        .bind(&record.session_id)
        .bind(&record.job_id)
        .fetch_one(&mut **tx)
        .await?;
        ensure!(
            existing == payload,
            "Project task configuration archive conflicts with this job"
        );
    }
    Ok(stored)
}

async fn read_transcript_from(
    connection: &mut SqliteConnection,
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
    .fetch_optional(&mut *connection)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let config = read_configuration(connection, session_id, job_id)
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

async fn read_configuration(
    connection: &mut SqliteConnection,
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
    .fetch_optional(&mut *connection)
    .await?;
    let authorization = if row.is_some() {
        VerifiedProjectTaskAuthorization::read_from(connection, session_id, job_id).await?
    } else {
        None
    };
    row.map(|row| {
        let schema: Option<String> = row.try_get("parameter_schema_id")?;
        let payload: Option<String> = row.try_get("snapshot_json")?;
        let identity_json: Option<String> = row.try_get("identity_json")?;
        let sha: Option<String> = row.try_get("snapshot_sha256")?;
        let schema = schema.context("Stored task schema identifier exceeds its byte limit")?;
        let sha = sha.context("Stored task digest has an invalid byte length")?;
        validate_digest(&sha)?;
        let payload = payload.context("Stored configuration exceeds its byte limit")?;
        let identity_json = identity_json.context("Stored task identity exceeds its byte limit")?;
        let identity: TaskParameterIdentity = serde_json::from_str(&identity_json)?;
        let record = parse_snapshot_with_project_scope(
            &schema,
            &payload,
            authorization
                .as_ref()
                .map(VerifiedProjectTaskAuthorization::compilation),
        )?;
        ensure!(
            record.session_id == session_id
                && record.job_id == job_id
                && record.identity == identity
                && record.snapshot_sha256 == sha
                && serde_json::to_string(&identity)? == identity_json,
            "Stored task configuration identity or digest is inconsistent"
        );
        if let Some(authorization) = &authorization {
            authorization.validate_child(&record)?;
        }
        Ok(record)
    })
    .transpose()
}

fn bounded_json(value: &impl Serialize) -> Result<String> {
    super::project_payload::bounded_json(value, MAX_TASK_CONFIGURATION_BYTES)
        .context("Task configuration exceeds its byte limit or is invalid")
}

fn canonical_generation(
    service: &GenerationParameterService,
    session_id: &str,
    job_id: &str,
    mut parameters: Value,
    scope: Option<&ProjectTaskCompilationScope>,
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
    let request = GenerationTaskRequest {
        session_id: session_id.into(),
        job_id: job_id.into(),
        parameters: serde_json::from_value(parameters)?,
    };
    Ok(match scope {
        Some(scope) => service.compile_with_project_scope(request, scope)?,
        None => service.compile(request)?,
    })
}

fn validate_generation(
    snapshot: &GenerationParameterSnapshot,
    scope: Option<&ProjectTaskCompilationScope>,
) -> Result<TaskParameterIdentity> {
    let service = GenerationParameterService::new(snapshot.profile, snapshot.binding.clone())?;
    let identity = service.identity()?;
    let effective = serde_json::to_value(&snapshot.parameters)?;
    let canonical = canonical_generation(
        &service,
        &snapshot.session_id,
        &snapshot.job_id,
        effective.clone(),
        scope,
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
            scope,
        )?;
        ensure!(
            serde_json::to_value(&previous.snapshot().parameters)? == before,
            "Previous generation parameters do not match their schema"
        );
    }
    Ok(identity)
}

fn parse_snapshot(schema: &str, payload: &str) -> Result<TaskConfigurationRecord> {
    parse_snapshot_with_project_scope(schema, payload, None)
}

fn parse_snapshot_with_project_scope(
    schema: &str,
    payload: &str,
    binding: Option<&ProjectCompilationBinding>,
) -> Result<TaskConfigurationRecord> {
    let scope = binding
        .map(ProjectCompilationBinding::domain_scope)
        .transpose()?;
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
            let identity = validate_generation(&value, scope.as_ref())?;
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
            match scope.as_ref() {
                Some(scope) => service.validate_snapshot_with_project_scope(&value, scope)?,
                None => service.validate_snapshot(&value)?,
            };
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
            match scope.as_ref() {
                Some(scope) => service.validate_snapshot_with_project_scope(&value, scope)?,
                None => service.validate_snapshot(&value)?,
            };
            (
                value.metadata.session_id.clone(),
                value.metadata.job_id.clone(),
                service.identity()?,
                bounded_json(&value)?,
                TaskConfigurationSnapshot::Qwen3Asr(Box::new(value)),
            )
        }
        Some((AudioTaskProtocol::StableAudioMusicOffline, profile)) => {
            let value: StableAudioMusicSnapshot = serde_json::from_str(payload)?;
            let service = StableAudioMusicParameterService::for_profile(
                profile,
                value.metadata.binding.clone(),
            )?;
            match scope.as_ref() {
                Some(scope) => service.validate_snapshot_with_project_scope(&value, scope)?,
                None => service.validate_snapshot(&value)?,
            };
            (
                value.metadata.session_id.clone(),
                value.metadata.job_id.clone(),
                service.identity()?,
                bounded_json(&value)?,
                TaskConfigurationSnapshot::StableAudioMusic(Box::new(value)),
            )
        }
        None => bail!("Unsupported persisted task parameter schema: {schema}"),
    };
    validate_id(&session_id)?;
    validate_id(&job_id)?;
    if let Some(scope) = &scope {
        ensure!(
            scope.child_session_id() == session_id && scope.child_job_id() == job_id,
            "Project compile scope belongs to another task"
        );
    }
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
        project_scope: binding.cloned(),
    })
}

#[cfg(test)]
#[path = "task_configuration_tests.rs"]
mod tests;
