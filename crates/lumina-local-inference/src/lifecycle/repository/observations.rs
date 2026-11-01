use anyhow::{anyhow, ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::io::Write;

use super::LifecycleRepository;
use crate::lifecycle::{
    Artifact, LifecycleErrorCode, ModelSourceError, Sha256Digest, SourceGgufObservation,
};

pub(super) const SCHEMA: &str = include_str!("migrations/0005.sql");
pub const GGUF_OBSERVATION_VERSION: &str = "gguf-source-observation-v1";
const MAX_OBSERVATION_JSON_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GgufObservationRecord {
    pub id: Sha256Digest,
    pub observation_version: String,
    pub observation: SourceGgufObservation,
    pub observed_at: i64,
}

struct ObservationWriter(Vec<u8>);

impl Write for ObservationWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_OBSERVATION_JSON_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "GGUF observation exceeds JSON storage budget",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn observation_id(observation: &SourceGgufObservation) -> Result<Sha256Digest> {
    Sha256Digest::of_serializable(&(GGUF_OBSERVATION_VERSION, observation.digest()?))
}

pub(super) fn read_by(
    connection: &Connection,
    id: &Sha256Digest,
) -> Result<Option<GgufObservationRecord>> {
    let row=connection.query_row("SELECT artifact_id, observation_version, observation_digest, header_digest, payload_digest, payload, observed_at FROM gguf_observations WHERE id = ?1",[id.as_str()],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,String>(5)?,row.get::<_,i64>(6)?))).optional()?;
    row.map(
        |(artifact_id, version, digest, header_digest, payload_digest, payload, observed_at)| {
            ensure!(
                version == GGUF_OBSERVATION_VERSION,
                "Stored GGUF observation version is unsupported"
            );
            ensure!(
                payload.len() <= MAX_OBSERVATION_JSON_BYTES
                    && Sha256Digest::of_bytes(payload.as_bytes()).as_str() == payload_digest,
                "Stored GGUF observation payload integrity failed"
            );
            let observation: SourceGgufObservation = serde_json::from_str(&payload)?;
            observation.validate()?;
            ensure!(
                observation.artifact.id == artifact_id
                    && observation.digest()?.as_str() == digest
                    && observation.header.header_digest.as_str() == header_digest
                    && observation_id(&observation)? == *id,
                "Stored GGUF observation binding integrity failed"
            );
            Ok(GgufObservationRecord {
                id: id.clone(),
                observation_version: version,
                observation,
                observed_at,
            })
        },
    )
    .transpose()
}

fn validate_catalog_artifact(
    connection: &Connection,
    observation: &SourceGgufObservation,
) -> Result<()> {
    let (digest, payload): (String, String) = connection.query_row(
        "SELECT digest, payload FROM catalog_artifacts WHERE id = ?1",
        [&observation.artifact.id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        Sha256Digest::of_bytes(payload.as_bytes()).as_str() == digest,
        "Catalog artifact payload integrity failed"
    );
    let artifact: Artifact = serde_json::from_str(&payload)?;
    ensure!(
        artifact == observation.artifact,
        "GGUF observation does not match its registered artifact"
    );
    Ok(())
}

impl LifecycleRepository {
    pub fn save_gguf_observation(
        &self,
        observation: &SourceGgufObservation,
        observed_at: i64,
    ) -> Result<GgufObservationRecord> {
        observation.validate()?;
        let id = observation_id(observation)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_catalog_artifact(&tx, observation)?;
        if let Some(existing) = read_by(&tx, &id)? {
            return Ok(existing);
        }
        let conflict:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM gguf_observations WHERE artifact_id = ?1 AND header_digest <> ?2)",params![observation.artifact.id,observation.header.header_digest.as_str()],|row|row.get(0))?;
        ensure!(
            !conflict,
            ModelSourceError {
                code: LifecycleErrorCode::RevisionChanged,
                http_status: None
            }
        );
        let mut writer = ObservationWriter(Vec::new());
        serde_json::to_writer(&mut writer, observation)?;
        let payload = String::from_utf8(writer.0)?;
        let digest = observation.digest()?;
        tx.execute("INSERT INTO gguf_observations (id, artifact_id, observation_version, observation_digest, header_digest, payload_digest, payload, observed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",params![id.as_str(),observation.artifact.id,GGUF_OBSERVATION_VERSION,digest.as_str(),observation.header.header_digest.as_str(),Sha256Digest::of_bytes(payload.as_bytes()).as_str(),payload,observed_at])?;
        tx.commit()?;
        Ok(GgufObservationRecord {
            id,
            observation_version: GGUF_OBSERVATION_VERSION.into(),
            observation: observation.clone(),
            observed_at,
        })
    }

    pub fn read_gguf_observation(
        &self,
        id: &Sha256Digest,
    ) -> Result<Option<GgufObservationRecord>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        read_by(&connection, id)
    }

    pub fn gguf_observations_for_artifact(
        &self,
        artifact_id: &str,
    ) -> Result<Vec<GgufObservationRecord>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let mut statement = connection.prepare(
            "SELECT id FROM gguf_observations WHERE artifact_id = ?1 ORDER BY observed_at, id",
        )?;
        let ids = statement
            .query_map([artifact_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| {
                read_by(&connection, &Sha256Digest::parse(id)?)?
                    .ok_or_else(|| anyhow!("GGUF observation disappeared"))
            })
            .collect()
    }
}
