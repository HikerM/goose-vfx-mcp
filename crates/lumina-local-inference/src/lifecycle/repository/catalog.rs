use anyhow::{anyhow, ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::BTreeMap;

use super::LifecycleRepository;
use crate::lifecycle::{
    Artifact, Component, ModelCatalogSnapshot, ModelDefinition, ModelVariant, RepositorySnapshot,
    Sha256Digest,
};

pub(super) const SCHEMA: &str = include_str!("migrations/0002.sql");

fn check_existing(
    connection: &Connection,
    query: &str,
    id: &str,
    value: &impl Serialize,
) -> Result<bool> {
    let stored: Option<(String, String)> = connection
        .query_row(query, [id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    let Some((digest, payload)) = stored else {
        return Ok(false);
    };
    let expected = Sha256Digest::of_serializable(value)?;
    ensure!(
        digest == expected.as_str() && Sha256Digest::of_bytes(payload.as_bytes()) == expected,
        "Catalog ID already contains different or corrupted content"
    );
    Ok(true)
}

fn read_record<T: DeserializeOwned + Serialize>(digest: String, payload: String) -> Result<T> {
    let value: T = serde_json::from_str(&payload)?;
    ensure!(
        Sha256Digest::of_serializable(&value)?.as_str() == digest,
        "Stored model catalog integrity check failed"
    );
    Ok(value)
}

fn read_map<T: DeserializeOwned + Serialize>(
    connection: &Connection,
    query: &str,
    model_id: &str,
) -> Result<BTreeMap<String, T>> {
    let mut statement = connection.prepare(query)?;
    let rows = statement.query_map([model_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    rows.map(|row| {
        let (id, digest, payload) = row?;
        Ok((id, read_record(digest, payload)?))
    })
    .collect()
}

impl LifecycleRepository {
    pub fn save_catalog(&self, catalog: &ModelCatalogSnapshot) -> Result<()> {
        catalog.validate()?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let digest = catalog.content_digest()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT digest FROM catalog_snapshots WHERE model_id = ?1",
                [&catalog.model.id],
                |row| row.get(0),
            )
            .optional()?;
        ensure!(
            existing
                .as_deref()
                .is_none_or(|stored| stored == digest.as_str()),
            "Model catalog identity already contains a different snapshot"
        );
        for (id, snapshot) in &catalog.snapshots {
            Self::store_snapshot(&tx, id, snapshot)?;
        }
        let model = &catalog.model;
        if !check_existing(
            &tx,
            "SELECT digest, payload FROM catalog_models WHERE id = ?1",
            &model.id,
            model,
        )? {
            tx.execute(
                "INSERT INTO catalog_models (id, digest, payload) VALUES (?1, ?2, ?3)",
                params![
                    model.id,
                    Sha256Digest::of_serializable(model)?.as_str(),
                    serde_json::to_string(model)?
                ],
            )?;
        }
        for id in catalog.snapshots.keys() {
            tx.execute("INSERT INTO model_snapshots (model_id, snapshot_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING", params![model.id, id])?;
        }
        for (id, artifact) in &catalog.artifacts {
            if !check_existing(
                &tx,
                "SELECT digest, payload FROM catalog_artifacts WHERE id = ?1",
                id,
                artifact,
            )? {
                tx.execute("INSERT INTO catalog_artifacts (id, snapshot_id, path, digest, payload) VALUES (?1, ?2, ?3, ?4, ?5)", params![id, artifact.snapshot_id, artifact.path.as_str(), Sha256Digest::of_serializable(artifact)?.as_str(), serde_json::to_string(artifact)?])?;
            }
            tx.execute("INSERT INTO model_artifacts (model_id, artifact_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING", params![model.id, id])?;
        }
        for (id, variant) in &catalog.variants {
            if !check_existing(
                &tx,
                "SELECT digest, payload FROM catalog_variants WHERE id = ?1",
                id,
                variant,
            )? {
                tx.execute("INSERT INTO catalog_variants (id, model_id, digest, payload) VALUES (?1, ?2, ?3, ?4)", params![id, model.id, Sha256Digest::of_serializable(variant)?.as_str(), serde_json::to_string(variant)?])?;
            }
            for artifact in &variant.artifact_ids {
                tx.execute("INSERT INTO variant_artifacts (variant_id, artifact_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING", params![id, artifact])?;
            }
        }
        for (id, component) in &catalog.components {
            if !check_existing(
                &tx,
                "SELECT digest, payload FROM catalog_components WHERE id = ?1",
                id,
                component,
            )? {
                tx.execute(
                    "INSERT INTO catalog_components (id, digest, payload) VALUES (?1, ?2, ?3)",
                    params![
                        id,
                        Sha256Digest::of_serializable(component)?.as_str(),
                        serde_json::to_string(component)?
                    ],
                )?;
            }
            tx.execute("INSERT INTO model_components (model_id, component_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING", params![model.id, id])?;
            for artifact in &component.artifact_ids {
                tx.execute("INSERT INTO component_artifacts (component_id, artifact_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING", params![id, artifact])?;
            }
        }
        tx.execute("INSERT INTO catalog_snapshots (model_id, digest) VALUES (?1, ?2) ON CONFLICT DO NOTHING", params![model.id, digest.as_str()])?;
        tx.commit()?;
        Ok(())
    }

    pub fn read_catalog(&self, model_id: &str) -> Result<Option<ModelCatalogSnapshot>> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction()?;
        let result = Self::read_catalog_by(&tx, model_id)?;
        tx.commit()?;
        Ok(result)
    }

    pub(super) fn read_catalog_by(
        connection: &Connection,
        model_id: &str,
    ) -> Result<Option<ModelCatalogSnapshot>> {
        let stored: Option<(String, String)> = connection
            .query_row(
                "SELECT digest, payload FROM catalog_models WHERE id = ?1",
                [model_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((digest, payload)) = stored else {
            return Ok(None);
        };
        let model: ModelDefinition = read_record(digest, payload)?;
        ensure!(model.id == model_id, "Stored model identity mismatch");
        let mut snapshots = BTreeMap::new();
        let mut stmt = connection.prepare("SELECT s.id, s.digest, s.payload FROM repository_snapshots s JOIN model_snapshots m ON m.snapshot_id = s.id WHERE m.model_id = ?1 ORDER BY s.id")?;
        let rows = stmt.query_map([model_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (id, digest, payload) = row?;
            let snapshot: RepositorySnapshot = serde_json::from_str(&payload)?;
            ensure!(
                snapshot.content_digest()?.as_str() == digest,
                "Stored source snapshot integrity check failed"
            );
            snapshots.insert(id, snapshot);
        }
        drop(stmt);
        let artifacts: BTreeMap<String, Artifact> = read_map(connection, "SELECT a.id, a.digest, a.payload FROM catalog_artifacts a JOIN model_artifacts m ON m.artifact_id = a.id WHERE m.model_id = ?1 ORDER BY a.id", model_id)?;
        let variants: BTreeMap<String, ModelVariant> = read_map(
            connection,
            "SELECT id, digest, payload FROM catalog_variants WHERE model_id = ?1 ORDER BY id",
            model_id,
        )?;
        let components: BTreeMap<String, Component> = read_map(connection, "SELECT c.id, c.digest, c.payload FROM catalog_components c JOIN model_components m ON m.component_id = c.id WHERE m.model_id = ?1 ORDER BY c.id", model_id)?;
        let catalog = ModelCatalogSnapshot {
            model,
            snapshots,
            artifacts,
            variants,
            components,
        };
        catalog.validate()?;
        let digest: String = connection.query_row(
            "SELECT digest FROM catalog_snapshots WHERE model_id = ?1",
            [model_id],
            |row| row.get(0),
        )?;
        ensure!(
            catalog.content_digest()?.as_str() == digest,
            "Stored model catalog snapshot is incomplete or corrupted"
        );
        Ok(Some(catalog))
    }
}
