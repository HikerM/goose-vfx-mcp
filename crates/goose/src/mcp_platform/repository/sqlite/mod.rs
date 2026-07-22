mod lifecycle_repository;
mod migrations;
mod records;
mod task_repository;

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Pool, Row, Sqlite, Transaction};

use crate::config::paths::Paths;
use crate::mcp_platform::domain::ManagedMcpState;
use crate::mcp_platform::error::{McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::repository::{
    ConnectionProjectionRecord, InsertOutcome, ManagedMcpRecord, ManagedVersionRecord,
    ManifestRecord, NewManagedMcp, PlanRecord, SavePlan,
};
use records::{
    decode, decode_managed_row, decode_managed_version_row, decode_manifest_row, decode_plan_row,
    encode, encode_optional, error, find_managed_by_identity, integrity_error, map_sqlx, not_found,
    plan_envelope_digest_for_save, repository_unavailable, revision_conflict, schema_objects,
    validate_manifest_proof, validate_plan_evidence, validate_plan_manifest,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseDiagnostics {
    pub foreign_keys: bool,
    pub journal_mode: String,
    pub busy_timeout_ms: i64,
    pub tables: Vec<String>,
    pub indexes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SqliteMcpPlatformRepository {
    pool: Pool<Sqlite>,
    database_path: Option<PathBuf>,
}

impl SqliteMcpPlatformRepository {
    pub async fn open_default() -> McpPlatformResult<Self> {
        Self::open_path(Paths::in_data_dir("mcp-platform/platform.db")).await
    }

    pub async fn open_path(path: impl AsRef<Path>) -> McpPlatformResult<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| repository_unavailable())?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(30))
            .journal_mode(SqliteJournalMode::Wal);
        Self::open_with_options(options, Some(path.to_path_buf()), 5).await
    }

    pub async fn open_url(url: &str) -> McpPlatformResult<Self> {
        let options = SqliteConnectOptions::from_str(url)
            .map_err(|_| repository_unavailable())?
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(30))
            .journal_mode(SqliteJournalMode::Wal);
        let max_connections = if url.contains(":memory:") { 1 } else { 5 };
        Self::open_with_options(options, None, max_connections).await
    }

    async fn open_with_options(
        options: SqliteConnectOptions,
        database_path: Option<PathBuf>,
        max_connections: u32,
    ) -> McpPlatformResult<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .connect_with(options)
            .await
            .map_err(map_sqlx)?;
        if let Err(error) = migrate(&pool).await {
            pool.close().await;
            return Err(error);
        }
        Ok(Self {
            pool,
            database_path,
        })
    }

    pub fn database_path(&self) -> Option<&Path> {
        self.database_path.as_deref()
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    pub async fn diagnostics(&self) -> McpPlatformResult<DatabaseDiagnostics> {
        let foreign_keys = sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
            .fetch_one(&self.pool)
            .await
            .map_err(map_sqlx)?
            == 1;
        let journal_mode = sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
            .fetch_one(&self.pool)
            .await
            .map_err(map_sqlx)?;
        let busy_timeout_ms = sqlx::query_scalar::<_, i64>("PRAGMA busy_timeout")
            .fetch_one(&self.pool)
            .await
            .map_err(map_sqlx)?;
        let tables = schema_objects(&self.pool, "table").await?;
        let indexes = schema_objects(&self.pool, "index").await?;
        Ok(DatabaseDiagnostics {
            foreign_keys,
            journal_mode,
            busy_timeout_ms,
            tables,
            indexes,
        })
    }

    pub async fn save_manifest(&self, record: &ManifestRecord) -> McpPlatformResult<InsertOutcome> {
        validate_manifest_proof(&record.proof, record.verified.digest())?;
        let mut tx = self.begin_immediate().await?;
        if let Some(row) = sqlx::query(
            r#"SELECT manifest_digest, mcp_id, version, canonical_bytes, proof_json,
                trust_tier_json, created_at_ms FROM manifest_blobs
                WHERE mcp_id = ? AND version = ?"#,
        )
        .bind(&record.verified.manifest().id)
        .bind(record.verified.manifest().version.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?
        {
            let existing = decode_manifest_row(&row)?;
            tx.commit().await.map_err(map_sqlx)?;
            return if existing.verified.digest() == record.verified.digest() {
                Ok(InsertOutcome::IdempotentReplay)
            } else {
                Err(error(
                    McpPlatformErrorCode::ManifestConflict,
                    "manifest identity already has different immutable content",
                ))
            };
        }

        sqlx::query(
            r#"INSERT INTO manifest_blobs (
                manifest_digest, mcp_id, version, canonical_bytes, proof_json, trust_tier_json,
                created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(record.verified.digest())
        .bind(&record.verified.manifest().id)
        .bind(record.verified.manifest().version.as_str())
        .bind(record.verified.canonical_json())
        .bind(encode(&record.proof)?)
        .bind(encode(&record.trust_tier)?)
        .bind(record.created_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(InsertOutcome::Inserted)
    }

    pub async fn get_manifest(&self, digest: &str) -> McpPlatformResult<ManifestRecord> {
        let row = sqlx::query(
            r#"SELECT manifest_digest, mcp_id, version, canonical_bytes, proof_json,
                trust_tier_json, created_at_ms FROM manifest_blobs WHERE manifest_digest = ?"#,
        )
        .bind(digest)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        decode_manifest_row(&row)
    }

    pub async fn list_manifests(&self) -> McpPlatformResult<Vec<ManifestRecord>> {
        let rows = sqlx::query(
            r#"SELECT manifest_digest, mcp_id, version, canonical_bytes, proof_json,
                trust_tier_json, created_at_ms FROM manifest_blobs
                ORDER BY mcp_id, version, manifest_digest"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        rows.iter().map(decode_manifest_row).collect()
    }

    pub async fn get_manifest_by_identity(
        &self,
        mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<ManifestRecord> {
        let row = sqlx::query(
            r#"SELECT manifest_digest, mcp_id, version, canonical_bytes, proof_json,
                trust_tier_json, created_at_ms FROM manifest_blobs
                WHERE mcp_id = ? AND version = ?"#,
        )
        .bind(mcp_id)
        .bind(version)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        decode_manifest_row(&row)
    }

    pub async fn create_managed_mcp(
        &self,
        new: &NewManagedMcp,
        now_ms: i64,
    ) -> McpPlatformResult<ManagedMcpRecord> {
        let mut tx = self.begin_immediate().await?;
        if let Some(existing) =
            find_managed_by_identity(&mut tx, &new.mcp_id, &new.installation_scope).await?
        {
            if existing == new.managed_mcp_id {
                tx.commit().await.map_err(map_sqlx)?;
                return self.get_managed_mcp(&existing).await;
            }
            return Err(error(
                McpPlatformErrorCode::IdempotencyConflict,
                "managed MCP identity already belongs to another stable identifier",
            ));
        }
        sqlx::query(
            r#"INSERT INTO managed_mcps (
                managed_mcp_id, mcp_id, installation_scope, state_json, revision,
                created_at_ms, updated_at_ms
            ) VALUES (?, ?, ?, ?, 0, ?, ?)"#,
        )
        .bind(&new.managed_mcp_id)
        .bind(&new.mcp_id)
        .bind(&new.installation_scope)
        .bind(encode(&ManagedMcpState::default())?)
        .bind(now_ms)
        .bind(now_ms)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_managed_mcp(&new.managed_mcp_id).await
    }

    pub async fn get_managed_mcp(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ManagedMcpRecord> {
        let row = sqlx::query(
            r#"SELECT managed_mcp_id, mcp_id, installation_scope, state_json, revision,
                created_at_ms, updated_at_ms FROM managed_mcps WHERE managed_mcp_id = ?"#,
        )
        .bind(managed_mcp_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let versions = self.list_managed_versions(managed_mcp_id).await?;
        decode_managed_row(&row, versions)
    }

    pub async fn update_managed_state(
        &self,
        managed_mcp_id: &str,
        expected_revision: i64,
        state: &ManagedMcpState,
        now_ms: i64,
    ) -> McpPlatformResult<ManagedMcpRecord> {
        let mut tx = self.begin_immediate().await?;
        let revision = sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM managed_mcps WHERE managed_mcp_id = ?",
        )
        .bind(managed_mcp_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        if revision != expected_revision {
            return Err(revision_conflict());
        }
        sqlx::query(
            "UPDATE managed_mcps SET state_json = ?, revision = revision + 1, updated_at_ms = ? WHERE managed_mcp_id = ?",
        )
        .bind(encode(state)?)
        .bind(now_ms)
        .bind(managed_mcp_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_managed_mcp(managed_mcp_id).await
    }

    pub async fn save_managed_version(
        &self,
        managed_mcp_id: &str,
        version: &ManagedVersionRecord,
    ) -> McpPlatformResult<InsertOutcome> {
        let mut tx = self.begin_immediate().await?;
        if let Some(row) = sqlx::query(
            r#"SELECT version, manifest_digest, installation_root, verified, active,
                adapter_evidence_json, created_at_ms FROM managed_versions
                WHERE managed_mcp_id = ? AND version = ?"#,
        )
        .bind(managed_mcp_id)
        .bind(&version.version)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?
        {
            let existing = decode_managed_version_row(&row)?;
            tx.commit().await.map_err(map_sqlx)?;
            return if &existing == version {
                Ok(InsertOutcome::IdempotentReplay)
            } else {
                Err(error(
                    McpPlatformErrorCode::IntegrityError,
                    "managed version already references a different manifest",
                ))
            };
        }
        sqlx::query(
            r#"INSERT INTO managed_versions (
                managed_mcp_id, version, manifest_digest, installation_root, verified, active,
                adapter_evidence_json, created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(managed_mcp_id)
        .bind(&version.version)
        .bind(&version.manifest_digest)
        .bind(&version.installation_root)
        .bind(version.verified)
        .bind(version.active)
        .bind(encode_optional(version.adapter_evidence.as_ref())?)
        .bind(version.created_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(InsertOutcome::Inserted)
    }

    async fn list_managed_versions(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Vec<ManagedVersionRecord>> {
        let rows = sqlx::query(
            r#"SELECT version, manifest_digest, installation_root, verified, active,
                adapter_evidence_json, created_at_ms FROM managed_versions
                WHERE managed_mcp_id = ? ORDER BY version"#,
        )
        .bind(managed_mcp_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        rows.iter().map(decode_managed_version_row).collect()
    }

    pub async fn put_connection_projection(
        &self,
        managed_mcp_id: &str,
        link_key: &str,
        projection: &crate::mcp_platform::plan::ConnectionProjection,
        expected_revision: Option<i64>,
        now_ms: i64,
    ) -> McpPlatformResult<ConnectionProjectionRecord> {
        let mut tx = self.begin_immediate().await?;
        let existing = sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM connection_projections WHERE managed_mcp_id = ?",
        )
        .bind(managed_mcp_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        match (existing, expected_revision) {
            (None, None) => {
                sqlx::query(
                    r#"INSERT INTO connection_projections (
                        managed_mcp_id, link_key, projection_json, revision, updated_at_ms
                    ) VALUES (?, ?, ?, 0, ?)"#,
                )
                .bind(managed_mcp_id)
                .bind(link_key)
                .bind(encode(projection)?)
                .bind(now_ms)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
            }
            (Some(actual), Some(expected)) if actual == expected => {
                sqlx::query(
                    r#"UPDATE connection_projections SET link_key = ?, projection_json = ?,
                        revision = revision + 1, updated_at_ms = ? WHERE managed_mcp_id = ?"#,
                )
                .bind(link_key)
                .bind(encode(projection)?)
                .bind(now_ms)
                .bind(managed_mcp_id)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
            }
            _ => return Err(revision_conflict()),
        }
        tx.commit().await.map_err(map_sqlx)?;
        self.get_connection_projection(managed_mcp_id).await
    }

    pub async fn get_connection_projection(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ConnectionProjectionRecord> {
        let row = sqlx::query(
            r#"SELECT managed_mcp_id, link_key, projection_json, revision, updated_at_ms,
                plan_id, manifest_digest, owner_task_id, projection_digest
                FROM connection_projections WHERE managed_mcp_id = ?"#,
        )
        .bind(managed_mcp_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let record = ConnectionProjectionRecord {
            managed_mcp_id: row.try_get("managed_mcp_id").map_err(map_sqlx)?,
            link_key: row.try_get("link_key").map_err(map_sqlx)?,
            projection: decode(
                &row.try_get::<String, _>("projection_json")
                    .map_err(map_sqlx)?,
            )?,
            revision: row.try_get("revision").map_err(map_sqlx)?,
            updated_at_ms: row.try_get("updated_at_ms").map_err(map_sqlx)?,
            plan_id: row.try_get("plan_id").map_err(map_sqlx)?,
            manifest_digest: row.try_get("manifest_digest").map_err(map_sqlx)?,
            owner_task_id: row.try_get("owner_task_id").map_err(map_sqlx)?,
            projection_digest: row.try_get("projection_digest").map_err(map_sqlx)?,
        };
        let legacy = record.plan_id.is_none()
            && record.manifest_digest.is_none()
            && record.owner_task_id.is_none()
            && record.projection_digest.is_empty();
        if !legacy {
            let (Some(plan_id), Some(manifest_digest), Some(_owner_task_id)) = (
                record.plan_id.as_deref(),
                record.manifest_digest.as_deref(),
                record.owner_task_id.as_deref(),
            ) else {
                return Err(integrity_error());
            };
            if record.projection_digest.is_empty() {
                return Err(integrity_error());
            }
            let managed = self.get_managed_mcp(managed_mcp_id).await?;
            let plan = self.get_plan(plan_id).await?;
            if plan.plan.manifest_digest() != manifest_digest
                || plan.target.mcp_id != managed.mcp_id
            {
                return Err(integrity_error());
            }
        }
        Ok(record)
    }

    pub async fn save_plan(&self, input: SavePlan<'_>) -> McpPlatformResult<PlanRecord> {
        input.plan.verify_integrity()?;
        if input.policy_evidence != input.plan.policy()
            || input.target.mcp_id != input.plan.manifest_id()
            || input.target.version != input.plan.manifest_version()
        {
            return Err(integrity_error());
        }
        validate_plan_evidence(
            input.plan,
            input.confirmation_evidence,
            input.created_at_ms,
            input.expires_at_ms,
        )?;
        let envelope_digest = plan_envelope_digest_for_save(&input)?;
        let mut tx = self.begin_immediate().await?;
        validate_plan_manifest(&mut tx, input.plan).await?;
        if let Some(row) = sqlx::query(
            r#"SELECT plan_id, plan_digest, envelope_digest, manifest_digest, operation,
                target_json, plan_json, policy_evidence_json, confirmation_evidence_json,
                expires_at_ms, idempotency_key, actor, created_at_ms
                FROM install_plans WHERE idempotency_key = ?"#,
        )
        .bind(input.idempotency_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?
        {
            let existing = decode_plan_row(&row)?;
            tx.commit().await.map_err(map_sqlx)?;
            return if existing.envelope_digest == envelope_digest {
                Ok(existing)
            } else {
                Err(error(
                    McpPlatformErrorCode::PlanConflict,
                    "plan idempotency key already has different immutable content",
                ))
            };
        }
        if sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM install_plans WHERE plan_id = ?)",
        )
        .bind(input.plan_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx)?
        {
            return Err(error(
                McpPlatformErrorCode::PlanConflict,
                "plan identifier already exists",
            ));
        }
        sqlx::query(
            r#"INSERT INTO install_plans (
                plan_id, plan_digest, envelope_digest, manifest_digest, operation,
                target_json, plan_json,
                policy_evidence_json, confirmation_evidence_json, expires_at_ms,
                idempotency_key, actor, created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(input.plan_id)
        .bind(input.plan.plan_digest())
        .bind(&envelope_digest)
        .bind(input.plan.manifest_digest())
        .bind(input.plan.operation().as_str())
        .bind(encode(input.target)?)
        .bind(encode(input.plan)?)
        .bind(encode(input.policy_evidence)?)
        .bind(encode(input.confirmation_evidence)?)
        .bind(input.expires_at_ms)
        .bind(input.idempotency_key)
        .bind(input.actor)
        .bind(input.created_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_plan(input.plan_id).await
    }

    pub async fn get_plan(&self, plan_id: &str) -> McpPlatformResult<PlanRecord> {
        let row = sqlx::query(
            r#"SELECT plan_id, plan_digest, envelope_digest, manifest_digest, operation,
                target_json, plan_json, policy_evidence_json, confirmation_evidence_json,
                expires_at_ms, idempotency_key, actor, created_at_ms
                FROM install_plans WHERE plan_id = ?"#,
        )
        .bind(plan_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let record = decode_plan_row(&row)?;
        let manifest = self.get_manifest(record.plan.manifest_digest()).await?;
        if manifest.verified.manifest().id != record.plan.manifest_id()
            || manifest.verified.manifest().version.as_str() != record.plan.manifest_version()
            || record.target.mcp_id != record.plan.manifest_id()
            || record.target.version != record.plan.manifest_version()
        {
            return Err(integrity_error());
        }
        Ok(record)
    }

    pub async fn get_plan_by_idempotency_key(
        &self,
        idempotency_key: &str,
    ) -> McpPlatformResult<Option<PlanRecord>> {
        let plan_id = sqlx::query_scalar::<_, String>(
            "SELECT plan_id FROM install_plans WHERE idempotency_key = ?",
        )
        .bind(idempotency_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        match plan_id {
            Some(plan_id) => self.get_plan(&plan_id).await.map(Some),
            None => Ok(None),
        }
    }

    async fn begin_immediate(&self) -> McpPlatformResult<Transaction<'_, Sqlite>> {
        self.pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(map_sqlx)
    }
}

async fn migrate(pool: &Pool<Sqlite>) -> McpPlatformResult<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(map_sqlx)?;
    let schema_exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_version')",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(map_sqlx)?;
    let current = if schema_exists {
        sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(version) FROM schema_version")
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx)?
            .unwrap_or(0)
    } else {
        0
    };
    if current > migrations::CURRENT_SCHEMA_VERSION {
        return Err(error(
            McpPlatformErrorCode::SchemaTooNew,
            "MCP platform database schema is newer than this build supports",
        ));
    }
    if !schema_exists {
        sqlx::query(
            r#"CREATE TABLE schema_version (
                version INTEGER PRIMARY KEY,
                applied_at_ms INTEGER NOT NULL
            )"#,
        )
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
    }
    if current < 1 {
        migrations::apply_v1(&mut tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (1, 0)")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 2 {
        migrations::apply_v2(&mut tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (2, 0)")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 3 {
        migrations::apply_v3(&mut tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (3, 0)")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx)?;
    }
    tx.commit().await.map_err(map_sqlx)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::mcp_platform::manifest::Auth;
    use crate::mcp_platform::plan::ConnectionProjection;

    use super::*;

    #[tokio::test]
    async fn v1_projection_migrates_through_v3_with_explicit_legacy_read_compatibility() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-platform.db");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        sqlx::query(
            "CREATE TABLE schema_version(version INTEGER PRIMARY KEY, applied_at_ms INTEGER NOT NULL)",
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        migrations::apply_v1(&mut transaction).await.unwrap();
        sqlx::query("INSERT INTO schema_version VALUES (1, 1)")
            .execute(&mut *transaction)
            .await
            .unwrap();
        let state = serde_json::to_string(&ManagedMcpState::default()).unwrap();
        sqlx::query(
            "INSERT INTO managed_mcps (managed_mcp_id, mcp_id, installation_scope, state_json, revision, created_at_ms, updated_at_ms) VALUES ('legacy-managed', 'legacy.example', 'user', ?, 0, 1, 1)",
        )
        .bind(state)
        .execute(&mut *transaction)
        .await
        .unwrap();
        let projection = ConnectionProjection::RemoteHttp {
            name: "legacy".to_string(),
            description: String::new(),
            uri: "https://legacy.example/mcp".to_string(),
            timeout_seconds: Some(5),
            auth: Auth::None,
        };
        sqlx::query(
            "INSERT INTO connection_projections (managed_mcp_id, link_key, projection_json, revision, updated_at_ms) VALUES ('legacy-managed', 'managed_mcp_legacy', ?, 0, 1)",
        )
        .bind(serde_json::to_string(&projection).unwrap())
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        pool.close().await;

        let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        let migrated = repository
            .get_connection_projection("legacy-managed")
            .await
            .unwrap();
        assert_eq!(migrated.projection, projection);
        assert!(migrated.plan_id.is_none());
        assert!(migrated.manifest_digest.is_none());
        assert!(migrated.owner_task_id.is_none());
        assert!(migrated.projection_digest.is_empty());
        let versions =
            sqlx::query_scalar::<_, i64>("SELECT version FROM schema_version ORDER BY version")
                .fetch_all(&repository.pool)
                .await
                .unwrap();
        assert_eq!(versions, [1, 2, 3]);
        let compensation_columns = sqlx::query("PRAGMA table_info(task_steps)")
            .fetch_all(&repository.pool)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|row| row.try_get::<String, _>("name").ok())
            .filter(|name| name.starts_with("compensation_"))
            .collect::<Vec<_>>();
        assert_eq!(
            compensation_columns,
            [
                "compensation_json",
                "compensation_status",
                "compensation_started_at_ms",
                "compensation_committed_at_ms"
            ]
        );
    }
}
