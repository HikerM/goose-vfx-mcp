use std::collections::BTreeSet;

use anyhow::{anyhow, bail, Result};
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite, Transaction};

use super::json::{parse_json_bytes, write_json_string, AstValue, ParserLimits};
use super::model::VerifiedSourceDocument;
use super::schema::{
    SOURCE_ONLY_INDEXES, SOURCE_ONLY_TABLES, SOURCE_ONLY_TRIGGERS, SOURCE_ONLY_VIEWS,
    SOURCE_SCHEMA_FENCE_ID, SOURCE_SCHEMA_MAX_VERSION, SOURCE_SCHEMA_MIN_VERSION,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    Active,
    Suspended,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseState {
    Active,
    Revoked,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceCatalogOperation {
    IngestSnapshot,
    VerifySnapshot,
    ActivateSource,
    RevokeRelease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationState {
    Pending,
    Applied,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopFlag {
    pub source_id: String,
    pub stop_required: bool,
    pub reason: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSource {
    pub source_id: String,
    pub source_name: String,
    pub issued_at_ms: i64,
    pub source_status: SourceState,
    pub stop_required: bool,
    pub release_count: usize,
    pub revocation_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRelease {
    pub source_id: String,
    pub release_id: String,
    pub mcp_id: String,
    pub version: String,
    pub manifest_kind: String,
    pub platform: String,
    pub architecture: String,
    pub variant: String,
    pub release_state: ReleaseState,
    pub revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSourceOperation {
    pub source_id: String,
    pub operation_id: String,
    pub operation_kind: SourceCatalogOperation,
    pub operation_state: OperationState,
    pub last_error: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerifiedDocumentRefreshWriteOutcome {
    Applied,
    IdempotentReplay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCatalogSafeView {
    pub source: StoredSource,
    pub releases: Vec<StoredRelease>,
    pub operations: Vec<StoredSourceOperation>,
}

impl SourceCatalogSafeView {
    pub fn to_json_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(b'{');
        write_json_string(&mut out, "operations");
        out.push(b':');
        write_operations_json(&mut out, &self.operations);
        out.push(b',');
        write_json_string(&mut out, "releases");
        out.push(b':');
        write_releases_json(&mut out, &self.releases);
        out.push(b',');
        write_json_string(&mut out, "source");
        out.push(b':');
        write_source_json(&mut out, &self.source);
        out.push(b'}');
        out
    }

    pub fn from_json_bytes(raw: &[u8]) -> Result<Self> {
        let value = parse_json_bytes(raw, ParserLimits::default())?;
        let mut object = take_object(value, "safe source catalog view")?;
        let operations = take_array(take_required(&mut object, "operations")?, "operations")?
            .into_iter()
            .map(StoredSourceOperation::from_ast)
            .collect::<Result<Vec<_>>>()?;
        let releases = take_array(take_required(&mut object, "releases")?, "releases")?
            .into_iter()
            .map(StoredRelease::from_ast)
            .collect::<Result<Vec<_>>>()?;
        let source = StoredSource::from_ast(take_required(&mut object, "source")?)?;
        reject_unknown_fields(object, "safe source catalog view")?;
        Ok(Self {
            source,
            releases,
            operations,
        })
    }
}

pub struct SourceCatalogDao {
    pool: Pool<Sqlite>,
}

impl SourceCatalogDao {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &Pool<Sqlite> {
        &self.pool
    }

    pub async fn store_verified_document(
        &self,
        document: &VerifiedSourceDocument,
        now_ms: i64,
    ) -> Result<()> {
        ensure_source_schema_compatible(&self.pool).await?;
        let mut tx = self.pool.begin().await?;
        store_verified_document_tx(&mut tx, document, now_ms).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn set_source_status(
        &self,
        source_id: &str,
        source_state: SourceState,
        now_ms: i64,
    ) -> Result<()> {
        ensure_source_schema_compatible(&self.pool).await?;
        sqlx::query(
            r#"UPDATE source_catalog_documents
               SET source_status = ?, updated_at_ms = ?
               WHERE source_id = ?"#,
        )
        .bind(source_state.as_sql())
        .bind(now_ms)
        .bind(source_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_release_status(
        &self,
        source_id: &str,
        release_id: &str,
        release_state: ReleaseState,
        now_ms: i64,
    ) -> Result<()> {
        ensure_source_schema_compatible(&self.pool).await?;
        sqlx::query(
            r#"UPDATE source_releases
               SET release_status = ?, updated_at_ms = ?
               WHERE source_id = ? AND release_id = ?"#,
        )
        .bind(release_state.as_sql())
        .bind(now_ms)
        .bind(source_id)
        .bind(release_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_stop_required(
        &self,
        source_id: &str,
        stop_required: bool,
        reason: Option<&str>,
        now_ms: i64,
    ) -> Result<()> {
        ensure_source_schema_compatible(&self.pool).await?;
        sqlx::query(
            r#"INSERT INTO source_stop_flags(source_id, stop_required, reason, updated_at_ms)
               VALUES (?, ?, ?, ?)
               ON CONFLICT(source_id) DO UPDATE SET
                 stop_required=excluded.stop_required,
                 reason=excluded.reason,
                 updated_at_ms=excluded.updated_at_ms"#,
        )
        .bind(source_id)
        .bind(i64::from(stop_required))
        .bind(reason)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_operation(&self, operation: &StoredSourceOperation) -> Result<()> {
        ensure_source_schema_compatible(&self.pool).await?;
        sqlx::query(
            r#"INSERT INTO source_operations(
                source_id, operation_id, operation_kind, operation_status,
                last_error, created_at_ms, updated_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(source_id, operation_id) DO UPDATE SET
                operation_kind=excluded.operation_kind,
                operation_status=excluded.operation_status,
                last_error=excluded.last_error,
                updated_at_ms=excluded.updated_at_ms"#,
        )
        .bind(&operation.source_id)
        .bind(&operation.operation_id)
        .bind(operation.operation_kind.as_sql())
        .bind(operation.operation_state.as_sql())
        .bind(&operation.last_error)
        .bind(operation.created_at_ms)
        .bind(operation.updated_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_releases_by_source(&self, source_id: &str) -> Result<Vec<StoredRelease>> {
        ensure_source_schema_compatible(&self.pool).await?;
        let rows = sqlx::query(
            r#"SELECT
                    r.source_id, r.release_id, r.mcp_id, r.version, r.manifest_kind,
                    r.platform, r.architecture, r.variant, r.release_status,
                    EXISTS(
                        SELECT 1
                        FROM source_revocations v
                        WHERE v.source_id = r.source_id AND v.release_id = r.release_id
                    ) AS revoked
               FROM source_releases r
               WHERE r.source_id = ?
               ORDER BY r.mcp_id, r.version, r.manifest_kind, r.platform, r.architecture, r.variant, r.release_id"#
        )
        .bind(source_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(map_release_row).collect()
    }

    pub async fn list_safe_view(&self, source_id: &str) -> Result<SourceCatalogSafeView> {
        ensure_source_schema_compatible(&self.pool).await?;
        let source_row = sqlx::query(
            r#"SELECT
                    d.source_id, d.source_name, d.issued_at_ms, d.source_status,
                    COALESCE(sf.stop_required, 0) AS stop_required,
                    (SELECT COUNT(*) FROM source_releases r WHERE r.source_id = d.source_id) AS release_count,
                    (SELECT COUNT(*) FROM source_revocations v WHERE v.source_id = d.source_id) AS revocation_count
               FROM source_catalog_documents d
               LEFT JOIN source_stop_flags sf ON sf.source_id = d.source_id
               WHERE d.source_id = ?"#
        )
        .bind(source_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| anyhow!("Unknown source_id `{source_id}`"))?;

        let issued_at_ms =
            checked_timestamp_ms(source_row.try_get("issued_at_ms")?, "issued_at_ms")?;
        let release_count =
            checked_usize_count(source_row.try_get("release_count")?, "release_count")?;
        let revocation_count =
            checked_usize_count(source_row.try_get("revocation_count")?, "revocation_count")?;
        let source = StoredSource {
            source_id: source_row.try_get("source_id")?,
            source_name: source_row.try_get("source_name")?,
            issued_at_ms,
            source_status: SourceState::from_sql(source_row.try_get::<&str, _>("source_status")?)?,
            stop_required: source_row.try_get::<i64, _>("stop_required")? != 0,
            release_count,
            revocation_count,
        };

        let releases = self.list_releases_by_source(source_id).await?;
        let operation_rows = sqlx::query(
            r#"SELECT
                    source_id, operation_id, operation_kind, operation_status,
                    last_error, created_at_ms, updated_at_ms
               FROM source_operations
               WHERE source_id = ?
               ORDER BY created_at_ms, operation_id"#,
        )
        .bind(source_id)
        .fetch_all(&self.pool)
        .await?;
        let operations = operation_rows
            .into_iter()
            .map(map_operation_row)
            .collect::<Result<Vec<_>>>()?;
        Ok(SourceCatalogSafeView {
            source,
            releases,
            operations,
        })
    }
}

pub(crate) async fn store_verified_document_tx(
    tx: &mut Transaction<'_, Sqlite>,
    document: &VerifiedSourceDocument,
    now_ms: i64,
) -> Result<()> {
    sqlx::query("DELETE FROM source_root_keys WHERE source_id = ?")
        .bind(&document.envelope.payload.source_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM source_signatures WHERE source_id = ?")
        .bind(&document.envelope.payload.source_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        r#"INSERT INTO source_catalog_documents(
            source_id, source_name, schema_version, issued_at_ms, source_status,
            raw_bytes, canonical_payload_bytes, canonical_envelope_bytes,
            raw_digest, canonical_digest, signed_digest, binding_digest, document_digest,
            created_at_ms, updated_at_ms
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(source_id) DO UPDATE SET
            source_name=excluded.source_name,
            schema_version=excluded.schema_version,
            issued_at_ms=excluded.issued_at_ms,
            raw_bytes=excluded.raw_bytes,
            canonical_payload_bytes=excluded.canonical_payload_bytes,
            canonical_envelope_bytes=excluded.canonical_envelope_bytes,
            raw_digest=excluded.raw_digest,
            canonical_digest=excluded.canonical_digest,
            signed_digest=excluded.signed_digest,
            binding_digest=excluded.binding_digest,
            document_digest=excluded.document_digest,
            updated_at_ms=excluded.updated_at_ms"#,
    )
    .bind(&document.envelope.payload.source_id)
    .bind(&document.envelope.payload.source_name)
    .bind(document.envelope.payload.schema_version)
    .bind(document.envelope.payload.issued_at_ms)
    .bind(SourceState::Active.as_sql())
    .bind(&document.raw_bytes)
    .bind(&document.canonical_payload_bytes)
    .bind(&document.canonical_envelope_bytes)
    .bind(&document.digests.raw_digest)
    .bind(&document.digests.canonical_digest)
    .bind(&document.digests.signed_digest)
    .bind(&document.digests.binding_digest)
    .bind(&document.digests.document_digest)
    .bind(now_ms)
    .bind(now_ms)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        r#"INSERT INTO source_root_policies(source_id, current_quorum, previous_quorum, has_previous)
           VALUES (?, ?, ?, ?)
           ON CONFLICT(source_id) DO UPDATE SET
             current_quorum=excluded.current_quorum,
             previous_quorum=excluded.previous_quorum,
             has_previous=excluded.has_previous"#,
    )
    .bind(&document.envelope.payload.source_id)
    .bind(document.envelope.payload.root.quorum)
    .bind(document.envelope.payload.root.previous.as_ref().map(|root| root.quorum))
    .bind(i64::from(document.envelope.payload.root.previous.is_some()))
    .execute(&mut **tx)
    .await?;

    for (index, key) in document.envelope.payload.root.keys.iter().enumerate() {
        sqlx::query(
            r#"INSERT INTO source_root_keys(source_id, root_set, kid, spki_der_b64u, key_order)
               VALUES (?, 'current', ?, ?, ?)"#,
        )
        .bind(&document.envelope.payload.source_id)
        .bind(&key.kid)
        .bind(&key.spki_der_b64u)
        .bind(index as i64)
        .execute(&mut **tx)
        .await?;
    }
    if let Some(previous) = &document.envelope.payload.root.previous {
        for (index, key) in previous.keys.iter().enumerate() {
            sqlx::query(
                r#"INSERT INTO source_root_keys(source_id, root_set, kid, spki_der_b64u, key_order)
                   VALUES (?, 'previous', ?, ?, ?)"#,
            )
            .bind(&document.envelope.payload.source_id)
            .bind(&key.kid)
            .bind(&key.spki_der_b64u)
            .bind(index as i64)
            .execute(&mut **tx)
            .await?;
        }
    }

    for (index, signature) in document.envelope.signatures.iter().enumerate() {
        sqlx::query(
            r#"INSERT INTO source_signatures(source_id, kid, sig_b64u, signature_order)
               VALUES (?, ?, ?, ?)"#,
        )
        .bind(&document.envelope.payload.source_id)
        .bind(&signature.kid)
        .bind(&signature.sig_b64u)
        .bind(index as i64)
        .execute(&mut **tx)
        .await?;
    }

    let incoming_release_ids = document
        .envelope
        .payload
        .snapshot
        .releases
        .iter()
        .map(|release| release.release_id.as_str())
        .collect::<BTreeSet<_>>();
    let existing_unrevoked_release_ids = sqlx::query_scalar::<_, String>(
        r#"SELECT release_id
           FROM source_releases release
           WHERE source_id = ?
             AND NOT EXISTS (
                SELECT 1
                FROM source_revocations revocation
                WHERE revocation.source_id = release.source_id
                  AND revocation.release_id = release.release_id
             )"#,
    )
    .bind(&document.envelope.payload.source_id)
    .fetch_all(&mut **tx)
    .await?;
    for release_id in existing_unrevoked_release_ids {
        if !incoming_release_ids.contains(release_id.as_str()) {
            sqlx::query("DELETE FROM source_releases WHERE source_id = ? AND release_id = ?")
                .bind(&document.envelope.payload.source_id)
                .bind(release_id)
                .execute(&mut **tx)
                .await?;
        }
    }

    for release in &document.envelope.payload.snapshot.releases {
        let release_state = if document
            .envelope
            .payload
            .snapshot
            .revocations
            .iter()
            .any(|revocation| revocation.release_id == release.release_id)
        {
            ReleaseState::Revoked
        } else {
            ReleaseState::Active
        };
        sqlx::query(
            r#"INSERT INTO source_releases(
                source_id, release_id, mcp_id, version, manifest_kind, platform,
                architecture, variant, release_status, created_at_ms, updated_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(source_id, release_id) DO UPDATE SET
                mcp_id=excluded.mcp_id,
                version=excluded.version,
                manifest_kind=excluded.manifest_kind,
                platform=excluded.platform,
                architecture=excluded.architecture,
                variant=excluded.variant,
                release_status=CASE
                    WHEN source_releases.release_status = 'revoked'
                      OR EXISTS (
                        SELECT 1
                        FROM source_revocations revocation
                        WHERE revocation.source_id = source_releases.source_id
                          AND revocation.release_id = source_releases.release_id
                      ) THEN 'revoked'
                    ELSE excluded.release_status
                END,
                updated_at_ms=excluded.updated_at_ms"#,
        )
        .bind(&document.envelope.payload.source_id)
        .bind(&release.release_id)
        .bind(&release.mcp_id)
        .bind(&release.version)
        .bind(&release.manifest_kind)
        .bind(&release.platform)
        .bind(&release.architecture)
        .bind(&release.variant)
        .bind(release_state.as_sql())
        .bind(now_ms)
        .bind(now_ms)
        .execute(&mut **tx)
        .await?;
    }

    for revocation in &document.envelope.payload.snapshot.revocations {
        sqlx::query(
            r#"INSERT INTO source_revocations(
                source_id, release_id, reason_code, revoked_at_ms, created_at_ms
            ) VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(source_id, release_id) DO NOTHING"#,
        )
        .bind(&document.envelope.payload.source_id)
        .bind(&revocation.release_id)
        .bind(&revocation.reason_code)
        .bind(revocation.revoked_at_ms)
        .bind(now_ms)
        .execute(&mut **tx)
        .await?;
    }

    sqlx::query(
        r#"INSERT INTO source_stop_flags(source_id, stop_required, reason, updated_at_ms)
           VALUES (?, 0, NULL, ?)
           ON CONFLICT(source_id) DO NOTHING"#,
    )
    .bind(&document.envelope.payload.source_id)
    .bind(now_ms)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

pub(crate) async fn store_verified_document_refresh_tx(
    tx: &mut Transaction<'_, Sqlite>,
    document: &VerifiedSourceDocument,
    now_ms: i64,
) -> Result<VerifiedDocumentRefreshWriteOutcome> {
    let existing = sqlx::query(
        r#"SELECT issued_at_ms, document_digest
           FROM source_catalog_documents
           WHERE source_id = ?"#,
    )
    .bind(&document.envelope.payload.source_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(existing) = existing else {
        bail!("Refresh source has no existing verified source document");
    };
    let issued_at_ms: i64 = existing.try_get("issued_at_ms")?;
    let document_digest: String = existing.try_get("document_digest")?;
    match document.envelope.payload.issued_at_ms.cmp(&issued_at_ms) {
        std::cmp::Ordering::Less => {
            bail!("Refresh document issued_at_ms is older than the stored document");
        }
        std::cmp::Ordering::Equal if document.digests.document_digest != document_digest => {
            bail!("Refresh document conflicts with the stored issued_at_ms");
        }
        std::cmp::Ordering::Equal => {
            return Ok(VerifiedDocumentRefreshWriteOutcome::IdempotentReplay);
        }
        std::cmp::Ordering::Greater => {}
    }
    store_verified_document_tx(tx, document, now_ms).await?;
    Ok(VerifiedDocumentRefreshWriteOutcome::Applied)
}

impl StoredSource {
    fn from_ast(value: AstValue) -> Result<Self> {
        let mut object = take_object(value, "safe source")?;
        let source_id = take_safe_string(take_required(&mut object, "source_id")?, "source_id")?;
        let source_name =
            take_safe_string(take_required(&mut object, "source_name")?, "source_name")?;
        let issued_at_ms =
            take_timestamp_ms(take_required(&mut object, "issued_at_ms")?, "issued_at_ms")?;
        let source_status = SourceState::from_sql(&take_string(
            take_required(&mut object, "source_status")?,
            "source_status",
        )?)?;
        let stop_required = take_bool(
            take_required(&mut object, "stop_required")?,
            "stop_required",
        )?;
        let release_count = take_usize_count(
            take_required(&mut object, "release_count")?,
            "release_count",
        )?;
        let revocation_count = take_usize_count(
            take_required(&mut object, "revocation_count")?,
            "revocation_count",
        )?;
        reject_unknown_fields(object, "safe source")?;
        Ok(Self {
            source_id,
            source_name,
            issued_at_ms,
            source_status,
            stop_required,
            release_count,
            revocation_count,
        })
    }
}

impl StoredRelease {
    fn from_ast(value: AstValue) -> Result<Self> {
        let mut object = take_object(value, "safe release")?;
        let source_id = take_safe_string(take_required(&mut object, "source_id")?, "source_id")?;
        let release_id = take_safe_string(take_required(&mut object, "release_id")?, "release_id")?;
        let mcp_id = take_safe_string(take_required(&mut object, "mcp_id")?, "mcp_id")?;
        let version = take_safe_string(take_required(&mut object, "version")?, "version")?;
        let manifest_kind = take_safe_string(
            take_required(&mut object, "manifest_kind")?,
            "manifest_kind",
        )?;
        let platform = take_safe_string(take_required(&mut object, "platform")?, "platform")?;
        let architecture =
            take_safe_string(take_required(&mut object, "architecture")?, "architecture")?;
        let variant = take_safe_string(take_required(&mut object, "variant")?, "variant")?;
        let release_state = ReleaseState::from_sql(&take_string(
            take_required(&mut object, "release_state")?,
            "release_state",
        )?)?;
        let revoked = take_bool(take_required(&mut object, "revoked")?, "revoked")?;
        reject_unknown_fields(object, "safe release")?;
        Ok(Self {
            source_id,
            release_id,
            mcp_id,
            version,
            manifest_kind,
            platform,
            architecture,
            variant,
            release_state,
            revoked,
        })
    }
}

impl StoredSourceOperation {
    fn from_ast(value: AstValue) -> Result<Self> {
        let mut object = take_object(value, "safe source operation")?;
        let source_id = take_safe_string(take_required(&mut object, "source_id")?, "source_id")?;
        let operation_id =
            take_safe_string(take_required(&mut object, "operation_id")?, "operation_id")?;
        let operation_kind = SourceCatalogOperation::from_sql(&take_string(
            take_required(&mut object, "operation_kind")?,
            "operation_kind",
        )?)?;
        let operation_state = OperationState::from_sql(&take_string(
            take_required(&mut object, "operation_state")?,
            "operation_state",
        )?)?;
        let created_at_ms = take_timestamp_ms(
            take_required(&mut object, "created_at_ms")?,
            "created_at_ms",
        )?;
        let updated_at_ms = take_timestamp_ms(
            take_required(&mut object, "updated_at_ms")?,
            "updated_at_ms",
        )?;
        reject_unknown_fields(object, "safe source operation")?;
        Ok(Self {
            source_id,
            operation_id,
            operation_kind,
            operation_state,
            last_error: None,
            created_at_ms,
            updated_at_ms,
        })
    }
}

pub async fn bootstrap_source_schema(pool: &Pool<Sqlite>) -> Result<()> {
    let mut tx = pool.begin().await?;
    apply_source_schema_v20(&mut tx).await?;
    apply_source_schema_v21(&mut tx).await?;
    tx.commit().await?;
    ensure_source_schema_compatible(pool).await
}

pub async fn ensure_source_schema_compatible(pool: &Pool<Sqlite>) -> Result<()> {
    let mut tx = pool.begin().await?;
    for table in SOURCE_ONLY_TABLES {
        if !table_exists_tx(&mut tx, table).await? {
            bail!("required source-only table `{table}` is missing");
        }
    }
    let ledger_versions =
        sqlx::query_scalar::<_, i64>("SELECT version FROM source_schema_ledger ORDER BY version")
            .fetch_all(&mut *tx)
            .await?;
    if ledger_versions != vec![SOURCE_SCHEMA_MIN_VERSION, SOURCE_SCHEMA_MAX_VERSION] {
        bail!("source schema ledger does not match this build");
    }
    let fence_rows = sqlx::query(
        r#"SELECT fence_id, min_version, max_version, schema_hash
           FROM source_schema_fence
           ORDER BY fence_id"#,
    )
    .fetch_all(&mut *tx)
    .await?;
    let [fence_row] = fence_rows.as_slice() else {
        bail!("source schema compatibility fence does not match this build");
    };
    let fence_id: String = fence_row.try_get("fence_id")?;
    let min_version: i64 = fence_row.try_get("min_version")?;
    let max_version: i64 = fence_row.try_get("max_version")?;
    let schema_hash: String = fence_row.try_get("schema_hash")?;
    let expected_hash = expected_source_schema_hash();
    let actual_hash = actual_source_schema_hash_tx(&mut tx).await?;
    if fence_id != SOURCE_SCHEMA_FENCE_ID
        || min_version != SOURCE_SCHEMA_MIN_VERSION
        || max_version != SOURCE_SCHEMA_MAX_VERSION
        || schema_hash != expected_hash
        || actual_hash != expected_hash
    {
        bail!("source schema compatibility fence does not match this build");
    }
    tx.commit().await?;
    Ok(())
}

fn map_release_row(row: SqliteRow) -> Result<StoredRelease> {
    Ok(StoredRelease {
        source_id: row.try_get("source_id")?,
        release_id: row.try_get("release_id")?,
        mcp_id: row.try_get("mcp_id")?,
        version: row.try_get("version")?,
        manifest_kind: row.try_get("manifest_kind")?,
        platform: row.try_get("platform")?,
        architecture: row.try_get("architecture")?,
        variant: row.try_get("variant")?,
        release_state: ReleaseState::from_sql(row.try_get::<&str, _>("release_status")?)?,
        revoked: row.try_get::<i64, _>("revoked")? != 0,
    })
}

fn map_operation_row(row: SqliteRow) -> Result<StoredSourceOperation> {
    let created_at_ms = checked_timestamp_ms(row.try_get("created_at_ms")?, "created_at_ms")?;
    let updated_at_ms = checked_timestamp_ms(row.try_get("updated_at_ms")?, "updated_at_ms")?;
    Ok(StoredSourceOperation {
        source_id: row.try_get("source_id")?,
        operation_id: row.try_get("operation_id")?,
        operation_kind: SourceCatalogOperation::from_sql(
            row.try_get::<&str, _>("operation_kind")?,
        )?,
        operation_state: OperationState::from_sql(row.try_get::<&str, _>("operation_status")?)?,
        last_error: None,
        created_at_ms,
        updated_at_ms,
    })
}

async fn table_exists_tx(tx: &mut sqlx::Transaction<'_, Sqlite>, table_name: &str) -> Result<bool> {
    schema_object_exists_tx(tx, "table", table_name).await
}

async fn schema_object_exists_tx(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    object_type: &str,
    object_name: &str,
) -> Result<bool> {
    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = ? AND name = ?",
    )
    .bind(object_type)
    .bind(object_name)
    .fetch_one(&mut **tx)
    .await?;
    Ok(exists > 0)
}

pub(crate) async fn ensure_source_schema_metadata_tx(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
) -> Result<()> {
    if !table_exists_tx(tx, "source_schema_ledger").await? {
        sqlx::query(SOURCE_SCHEMA_LEDGER_DDL)
            .execute(&mut **tx)
            .await?;
    }
    if !table_exists_tx(tx, "source_schema_fence").await? {
        sqlx::query(SOURCE_SCHEMA_FENCE_DDL)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query(
        r#"INSERT INTO source_schema_ledger(version, applied_at_ms)
           VALUES (?, 0)
           ON CONFLICT(version) DO NOTHING"#,
    )
    .bind(SOURCE_SCHEMA_MIN_VERSION)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        r#"INSERT INTO source_schema_ledger(version, applied_at_ms)
           VALUES (?, 0)
           ON CONFLICT(version) DO NOTHING"#,
    )
    .bind(SOURCE_SCHEMA_MAX_VERSION)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        r#"INSERT INTO source_schema_fence(fence_id, min_version, max_version, schema_hash)
           VALUES (?, ?, ?, ?)
           ON CONFLICT(fence_id) DO UPDATE SET
             min_version=excluded.min_version,
             max_version=excluded.max_version,
             schema_hash=excluded.schema_hash"#,
    )
    .bind(SOURCE_SCHEMA_FENCE_ID)
    .bind(SOURCE_SCHEMA_MIN_VERSION)
    .bind(SOURCE_SCHEMA_MAX_VERSION)
    .bind(expected_source_schema_hash())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn apply_source_schema_v20(tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
    for object in source_schema_v20_objects() {
        if !schema_object_exists_tx(tx, object.object_type, object.name).await? {
            sqlx::query(object.sql).execute(&mut **tx).await?;
        }
    }
    Ok(())
}

async fn apply_source_schema_v21(tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<()> {
    ensure_source_schema_metadata_tx(tx).await?;
    for object in source_schema_v21_objects() {
        if !schema_object_exists_tx(tx, object.object_type, object.name).await? {
            sqlx::query(object.sql).execute(&mut **tx).await?;
        }
    }
    Ok(())
}

#[derive(Debug)]
struct SchemaObject {
    object_type: String,
    name: String,
    sql: String,
}

#[derive(Debug, Clone, Copy)]
struct SchemaInstallObject {
    object_type: &'static str,
    name: &'static str,
    sql: &'static str,
}

#[derive(Debug)]
struct SourceSchemaRow {
    object_type: String,
    name: String,
    table_name: String,
    sql: String,
}
const SOURCE_CATALOG_DOCUMENTS_DDL: &str = r#"CREATE TABLE source_catalog_documents (
    source_id TEXT PRIMARY KEY,
    source_name TEXT NOT NULL,
    schema_version INTEGER NOT NULL CHECK(schema_version = 1),
    issued_at_ms INTEGER NOT NULL,
    source_status TEXT NOT NULL CHECK(source_status IN ('active','suspended','revoked')),
    raw_bytes BLOB NOT NULL,
    canonical_payload_bytes BLOB NOT NULL,
    canonical_envelope_bytes BLOB NOT NULL,
    raw_digest TEXT NOT NULL CHECK(length(raw_digest) = 64),
    canonical_digest TEXT NOT NULL CHECK(length(canonical_digest) = 64),
    signed_digest TEXT NOT NULL CHECK(length(signed_digest) = 64),
    binding_digest TEXT NOT NULL CHECK(length(binding_digest) = 64),
    document_digest TEXT NOT NULL CHECK(length(document_digest) = 64),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
)"#;
const SOURCE_ROOT_POLICIES_DDL: &str = r#"CREATE TABLE source_root_policies (
    source_id TEXT PRIMARY KEY REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
    current_quorum INTEGER NOT NULL CHECK(current_quorum > 0),
    previous_quorum INTEGER CHECK(previous_quorum IS NULL OR previous_quorum > 0),
    has_previous INTEGER NOT NULL CHECK(has_previous IN (0,1))
)"#;
const SOURCE_ROOT_KEYS_DDL: &str = r#"CREATE TABLE source_root_keys (
    source_id TEXT NOT NULL REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
    root_set TEXT NOT NULL CHECK(root_set IN ('current','previous')),
    kid TEXT NOT NULL,
    spki_der_b64u TEXT NOT NULL,
    key_order INTEGER NOT NULL CHECK(key_order >= 0),
    PRIMARY KEY(source_id, root_set, kid)
)"#;
const SOURCE_SIGNATURES_DDL: &str = r#"CREATE TABLE source_signatures (
    source_id TEXT NOT NULL REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
    kid TEXT NOT NULL,
    sig_b64u TEXT NOT NULL,
    signature_order INTEGER NOT NULL CHECK(signature_order >= 0),
    PRIMARY KEY(source_id, kid)
)"#;
const SOURCE_RELEASES_DDL: &str = r#"CREATE TABLE source_releases (
    source_id TEXT NOT NULL REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
    release_id TEXT NOT NULL,
    mcp_id TEXT NOT NULL,
    version TEXT NOT NULL,
    manifest_kind TEXT NOT NULL,
    platform TEXT NOT NULL,
    architecture TEXT NOT NULL,
    variant TEXT NOT NULL,
    release_status TEXT NOT NULL CHECK(release_status IN ('active','revoked','superseded')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY(source_id, release_id),
    UNIQUE(source_id, mcp_id, version, manifest_kind, platform, architecture, variant)
)"#;
const SOURCE_REVOCATIONS_DDL: &str = r#"CREATE TABLE source_revocations (
    source_id TEXT NOT NULL,
    release_id TEXT NOT NULL,
    reason_code TEXT NOT NULL,
    revoked_at_ms INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(source_id, release_id),
    FOREIGN KEY(source_id, release_id) REFERENCES source_releases(source_id, release_id) ON DELETE RESTRICT
)"#;
const SOURCE_OPERATIONS_DDL: &str = r#"CREATE TABLE source_operations (
    source_id TEXT NOT NULL REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
    operation_id TEXT NOT NULL,
    operation_kind TEXT NOT NULL CHECK(operation_kind IN ('ingest_snapshot','verify_snapshot','activate_source','revoke_release')),
    operation_status TEXT NOT NULL CHECK(operation_status IN ('pending','applied','failed','stopped')),
    last_error TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY(source_id, operation_id)
)"#;
const SOURCE_STOP_FLAGS_DDL: &str = r#"CREATE TABLE source_stop_flags (
    source_id TEXT PRIMARY KEY REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
    stop_required INTEGER NOT NULL CHECK(stop_required IN (0,1)),
    reason TEXT,
    updated_at_ms INTEGER NOT NULL
)"#;
const SOURCE_SCHEMA_LEDGER_DDL: &str = r#"CREATE TABLE source_schema_ledger (
    version INTEGER PRIMARY KEY CHECK(version IN (20,21)),
    applied_at_ms INTEGER NOT NULL
)"#;
const SOURCE_SCHEMA_FENCE_DDL: &str = r#"CREATE TABLE source_schema_fence (
    fence_id TEXT PRIMARY KEY,
    min_version INTEGER NOT NULL,
    max_version INTEGER NOT NULL,
    schema_hash TEXT NOT NULL CHECK(length(schema_hash) = 64)
)"#;
const SOURCE_RELEASES_LOOKUP_INDEX_DDL: &str =
    "CREATE INDEX source_releases_lookup ON source_releases(source_id, mcp_id, version)";
const SOURCE_OPERATIONS_STATUS_INDEX_DDL: &str =
    "CREATE INDEX source_operations_status ON source_operations(source_id, operation_status)";

const SAFE_DTO_REDACTION: &str = "redacted";
const SAFE_DTO_FORBIDDEN_TOKENS: &[&str] = &[
    "url",
    "uri",
    "digest",
    "proof",
    "kid",
    "signature",
    "anchor",
    "witness",
    "credential",
    "handle",
    "provenance",
];

async fn actual_source_schema_hash_tx(tx: &mut sqlx::Transaction<'_, Sqlite>) -> Result<String> {
    let rows = sqlx::query(
        r#"SELECT type, name, tbl_name, sql
           FROM sqlite_master
           WHERE type IN ('table', 'index', 'trigger', 'view')
             AND sql IS NOT NULL
           ORDER BY type, name"#,
    )
    .fetch_all(&mut **tx)
    .await?;
    let source_rows = rows
        .into_iter()
        .map(|row| {
            Ok(SourceSchemaRow {
                object_type: row.try_get("type")?,
                name: row.try_get("name")?,
                table_name: row.try_get("tbl_name")?,
                sql: row.try_get("sql")?,
            })
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(is_source_related_schema_row)
        .collect::<Vec<_>>();
    let objects = source_rows
        .iter()
        .map(|row| SchemaObject {
            object_type: row.object_type.clone(),
            name: row.name.clone(),
            sql: row.sql.clone(),
        })
        .collect::<Vec<_>>();
    assert_source_schema_object_names(&objects, "table", SOURCE_ONLY_TABLES)?;
    assert_source_schema_object_names(&objects, "index", SOURCE_ONLY_INDEXES)?;
    assert_source_schema_object_names(&objects, "trigger", SOURCE_ONLY_TRIGGERS)?;
    assert_source_schema_object_names(&objects, "view", SOURCE_ONLY_VIEWS)?;
    let actual_objects = objects
        .iter()
        .map(|object| (object.object_type.clone(), object.name.clone()))
        .collect::<BTreeSet<_>>();
    let expected_objects = expected_source_schema_objects()
        .into_iter()
        .map(|object| (object.object_type, object.name))
        .collect::<BTreeSet<_>>();
    if actual_objects != expected_objects {
        bail!("source schema object set does not match this build");
    }
    Ok(schema_object_fingerprint(&objects))
}

fn expected_source_schema_hash() -> String {
    let mut objects = expected_source_schema_objects();
    objects.sort_by(|left, right| {
        (&left.object_type, &left.name).cmp(&(&right.object_type, &right.name))
    });
    schema_object_fingerprint(&objects)
}

fn schema_object_fingerprint(objects: &[SchemaObject]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"verified_source_catalog:schema:");
    for object in objects {
        hasher.update(object.object_type.as_bytes());
        hasher.update([0]);
        hasher.update(object.name.as_bytes());
        hasher.update([0]);
        hasher.update(normalize_schema_sql(&object.sql).as_bytes());
        hasher.update([0xff]);
    }
    hex_lower(&hasher.finalize())
}

fn source_schema_v20_objects() -> &'static [SchemaInstallObject] {
    &[
        SchemaInstallObject {
            object_type: "table",
            name: "source_catalog_documents",
            sql: SOURCE_CATALOG_DOCUMENTS_DDL,
        },
        SchemaInstallObject {
            object_type: "table",
            name: "source_root_policies",
            sql: SOURCE_ROOT_POLICIES_DDL,
        },
        SchemaInstallObject {
            object_type: "table",
            name: "source_root_keys",
            sql: SOURCE_ROOT_KEYS_DDL,
        },
        SchemaInstallObject {
            object_type: "table",
            name: "source_signatures",
            sql: SOURCE_SIGNATURES_DDL,
        },
        SchemaInstallObject {
            object_type: "table",
            name: "source_releases",
            sql: SOURCE_RELEASES_DDL,
        },
        SchemaInstallObject {
            object_type: "table",
            name: "source_revocations",
            sql: SOURCE_REVOCATIONS_DDL,
        },
        SchemaInstallObject {
            object_type: "table",
            name: "source_operations",
            sql: SOURCE_OPERATIONS_DDL,
        },
        SchemaInstallObject {
            object_type: "table",
            name: "source_stop_flags",
            sql: SOURCE_STOP_FLAGS_DDL,
        },
    ]
}

fn source_schema_v21_objects() -> &'static [SchemaInstallObject] {
    &[
        SchemaInstallObject {
            object_type: "index",
            name: "source_releases_lookup",
            sql: SOURCE_RELEASES_LOOKUP_INDEX_DDL,
        },
        SchemaInstallObject {
            object_type: "index",
            name: "source_operations_status",
            sql: SOURCE_OPERATIONS_STATUS_INDEX_DDL,
        },
    ]
}

fn expected_source_schema_objects() -> Vec<SchemaObject> {
    let mut objects = source_schema_v20_objects()
        .iter()
        .map(schema_object_from_install)
        .collect::<Vec<_>>();
    objects.extend([
        SchemaObject {
            object_type: "table".to_string(),
            name: "source_schema_ledger".to_string(),
            sql: SOURCE_SCHEMA_LEDGER_DDL.to_string(),
        },
        SchemaObject {
            object_type: "table".to_string(),
            name: "source_schema_fence".to_string(),
            sql: SOURCE_SCHEMA_FENCE_DDL.to_string(),
        },
    ]);
    objects.extend(
        source_schema_v21_objects()
            .iter()
            .map(schema_object_from_install),
    );
    objects
}

fn is_source_related_schema_row(row: &SourceSchemaRow) -> bool {
    row.name.starts_with("source_")
        || SOURCE_ONLY_TABLES.contains(&row.table_name.as_str())
        || sql_mentions_source_only_table_identifier(&row.sql)
}

fn sql_mentions_source_only_table_identifier(sql: &str) -> bool {
    let tokens = tokenize_sql(sql);
    let mut index = 0;
    let mut pending_delete_target = false;
    let mut trigger_header_active = false;
    let mut create_keyword_active = false;

    while let Some(token) = tokens.get(index) {
        let SqlToken::Word(word) = token else {
            index += 1;
            continue;
        };
        if word.eq_ignore_ascii_case("CREATE") {
            create_keyword_active = true;
            index += 1;
            continue;
        }
        if create_keyword_active && word.eq_ignore_ascii_case("TRIGGER") {
            trigger_header_active = true;
            create_keyword_active = false;
            index += 1;
            continue;
        }
        create_keyword_active = false;

        if pending_delete_target {
            pending_delete_target = false;
            if word.eq_ignore_ascii_case("FROM")
                && source_table_reference_after(&tokens, index + 1, true)
            {
                return true;
            }
        }

        if word.eq_ignore_ascii_case("BEGIN") {
            trigger_header_active = false;
        }

        if word.eq_ignore_ascii_case("DELETE") {
            pending_delete_target = true;
            index += 1;
            continue;
        }
        if word.eq_ignore_ascii_case("FROM") || word.eq_ignore_ascii_case("JOIN") {
            if source_table_reference_after(&tokens, index + 1, true) {
                return true;
            }
            index += 1;
            continue;
        }
        if word.eq_ignore_ascii_case("INTO")
            || word.eq_ignore_ascii_case("UPDATE")
            || word.eq_ignore_ascii_case("REFERENCES")
            || word.eq_ignore_ascii_case("TABLE")
        {
            if source_table_reference_after(&tokens, index + 1, false) {
                return true;
            }
            index += 1;
            continue;
        }
        if trigger_header_active
            && word.eq_ignore_ascii_case("ON")
            && source_table_reference_after(&tokens, index + 1, false)
        {
            return true;
        }
        index += 1;
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SqlToken {
    Word(String),
    QuotedIdentifier(String),
    SingleQuoted(String),
    Dot,
    Comma,
    OpenParen,
    CloseParen,
    Semicolon,
    Other,
}

fn tokenize_sql(sql: &str) -> Vec<SqlToken> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b' ' | b'\t' | b'\r' | b'\n' => {
                index += 1;
            }
            b'\'' => {
                let (identifier, next_index) =
                    read_sql_doubled_quoted_identifier(bytes, index + 1, b'\'');
                tokens.push(SqlToken::SingleQuoted(identifier));
                index = next_index;
            }
            b'"' => {
                let (identifier, next_index) =
                    read_sql_doubled_quoted_identifier(bytes, index + 1, b'"');
                tokens.push(SqlToken::QuotedIdentifier(identifier));
                index = next_index;
            }
            b'`' => {
                let (identifier, next_index) =
                    read_sql_doubled_quoted_identifier(bytes, index + 1, b'`');
                tokens.push(SqlToken::QuotedIdentifier(identifier));
                index = next_index;
            }
            b'[' => {
                let (identifier, next_index) = read_sql_bracket_quoted_identifier(bytes, index + 1);
                tokens.push(SqlToken::QuotedIdentifier(identifier));
                index = next_index;
            }
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                index = skip_sql_line_comment(bytes, index + 2);
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_sql_block_comment(bytes, index + 2);
            }
            b'.' => {
                tokens.push(SqlToken::Dot);
                index += 1;
            }
            b',' => {
                tokens.push(SqlToken::Comma);
                index += 1;
            }
            b'(' => {
                tokens.push(SqlToken::OpenParen);
                index += 1;
            }
            b')' => {
                tokens.push(SqlToken::CloseParen);
                index += 1;
            }
            b';' => {
                tokens.push(SqlToken::Semicolon);
                index += 1;
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let start = index;
                index += 1;
                while index < bytes.len() && is_sql_identifier_continue(bytes[index]) {
                    index += 1;
                }
                tokens.push(SqlToken::Word(sql[start..index].to_string()));
            }
            _ => {
                tokens.push(SqlToken::Other);
                index += 1;
            }
        }
    }
    tokens
}

fn source_table_reference_after(
    tokens: &[SqlToken],
    mut index: usize,
    allow_comma_list: bool,
) -> bool {
    loop {
        let Some(token) = tokens.get(index) else {
            return false;
        };
        if matches!(token, SqlToken::OpenParen) {
            return false;
        }
        let Some(next_index) = scan_table_reference(tokens, index) else {
            return false;
        };
        if next_index == usize::MAX {
            return true;
        }
        if !allow_comma_list {
            return false;
        }
        match tokens.get(next_index) {
            Some(SqlToken::Comma) => {
                index = next_index + 1;
            }
            _ => {
                return false;
            }
        }
    }
}

fn scan_table_reference(tokens: &[SqlToken], index: usize) -> Option<usize> {
    let mut next_index = scan_identifier_qualified_reference(tokens, index)?;
    if matches!(tokens.get(next_index), Some(SqlToken::Word(word)) if word.eq_ignore_ascii_case("AS"))
    {
        next_index += 1;
    }
    if token_can_be_alias(tokens.get(next_index)) {
        next_index += 1;
    }
    Some(next_index)
}

fn scan_identifier_qualified_reference(tokens: &[SqlToken], mut index: usize) -> Option<usize> {
    let identifier = token_as_table_identifier(tokens.get(index)?)?;
    if is_source_only_table_identifier(identifier) {
        return Some(usize::MAX);
    }
    index += 1;
    while matches!(tokens.get(index), Some(SqlToken::Dot)) {
        index += 1;
        let identifier = token_as_table_identifier(tokens.get(index)?)?;
        if is_source_only_table_identifier(identifier) {
            return Some(usize::MAX);
        }
        index += 1;
    }
    Some(index)
}

fn token_as_table_identifier(token: &SqlToken) -> Option<&str> {
    match token {
        SqlToken::Word(identifier)
        | SqlToken::QuotedIdentifier(identifier)
        | SqlToken::SingleQuoted(identifier) => Some(identifier),
        _ => None,
    }
}

fn token_can_be_alias(token: Option<&SqlToken>) -> bool {
    matches!(
        token,
        Some(SqlToken::Word(_) | SqlToken::QuotedIdentifier(_) | SqlToken::SingleQuoted(_))
    )
}

fn is_source_only_table_identifier(identifier: &str) -> bool {
    SOURCE_ONLY_TABLES
        .iter()
        .any(|table| identifier.eq_ignore_ascii_case(table))
}

fn is_sql_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn read_sql_doubled_quoted_identifier(
    bytes: &[u8],
    mut index: usize,
    delimiter: u8,
) -> (String, usize) {
    let mut identifier = Vec::new();
    while index < bytes.len() {
        if bytes[index] == delimiter {
            if bytes.get(index + 1) == Some(&delimiter) {
                identifier.push(delimiter);
                index += 2;
            } else {
                return (String::from_utf8_lossy(&identifier).into_owned(), index + 1);
            }
        } else {
            identifier.push(bytes[index]);
            index += 1;
        }
    }
    (
        String::from_utf8_lossy(&identifier).into_owned(),
        bytes.len(),
    )
}

fn read_sql_bracket_quoted_identifier(bytes: &[u8], mut index: usize) -> (String, usize) {
    let mut identifier = Vec::new();
    while index < bytes.len() {
        if bytes[index] == b']' {
            return (String::from_utf8_lossy(&identifier).into_owned(), index + 1);
        }
        identifier.push(bytes[index]);
        index += 1;
    }
    (
        String::from_utf8_lossy(&identifier).into_owned(),
        bytes.len(),
    )
}

fn skip_sql_line_comment(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() {
        if bytes[index] == b'\n' {
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

fn skip_sql_block_comment(bytes: &[u8], mut index: usize) -> usize {
    while index + 1 < bytes.len() {
        if bytes[index] == b'*' && bytes[index + 1] == b'/' {
            return index + 2;
        }
        index += 1;
    }
    bytes.len()
}

fn schema_object_from_install(object: &SchemaInstallObject) -> SchemaObject {
    SchemaObject {
        object_type: object.object_type.to_string(),
        name: object.name.to_string(),
        sql: object.sql.to_string(),
    }
}

fn assert_source_schema_object_names(
    objects: &[SchemaObject],
    object_type: &str,
    expected_names: &[&str],
) -> Result<()> {
    let actual_names = objects
        .iter()
        .filter(|object| object.object_type == object_type)
        .map(|object| object.name.as_str())
        .collect::<BTreeSet<_>>();
    let expected_names = expected_names.iter().copied().collect::<BTreeSet<_>>();
    if actual_names != expected_names {
        bail!("source schema {object_type} set does not match this build");
    }
    Ok(())
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn write_source_json(out: &mut Vec<u8>, source: &StoredSource) {
    out.push(b'{');
    write_json_string(out, "issued_at_ms");
    out.push(b':');
    out.extend_from_slice(source.issued_at_ms.to_string().as_bytes());
    out.push(b',');
    write_json_string(out, "release_count");
    out.push(b':');
    out.extend_from_slice(source.release_count.to_string().as_bytes());
    out.push(b',');
    write_json_string(out, "revocation_count");
    out.push(b':');
    out.extend_from_slice(source.revocation_count.to_string().as_bytes());
    out.push(b',');
    write_json_string(out, "source_id");
    out.push(b':');
    write_safe_dto_string(out, "source_id", &source.source_id);
    out.push(b',');
    write_json_string(out, "source_name");
    out.push(b':');
    write_safe_dto_string(out, "source_name", &source.source_name);
    out.push(b',');
    write_json_string(out, "source_status");
    out.push(b':');
    write_json_string(out, source.source_status.as_sql());
    out.push(b',');
    write_json_string(out, "stop_required");
    out.push(b':');
    out.extend_from_slice(if source.stop_required {
        b"true"
    } else {
        b"false"
    });
    out.push(b'}');
}

fn write_releases_json(out: &mut Vec<u8>, releases: &[StoredRelease]) {
    out.push(b'[');
    for (index, release) in releases.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        out.push(b'{');
        write_json_string(out, "architecture");
        out.push(b':');
        write_safe_dto_string(out, "architecture", &release.architecture);
        out.push(b',');
        write_json_string(out, "manifest_kind");
        out.push(b':');
        write_safe_dto_string(out, "manifest_kind", &release.manifest_kind);
        out.push(b',');
        write_json_string(out, "mcp_id");
        out.push(b':');
        write_safe_dto_string(out, "mcp_id", &release.mcp_id);
        out.push(b',');
        write_json_string(out, "platform");
        out.push(b':');
        write_safe_dto_string(out, "platform", &release.platform);
        out.push(b',');
        write_json_string(out, "release_id");
        out.push(b':');
        write_safe_dto_string(out, "release_id", &release.release_id);
        out.push(b',');
        write_json_string(out, "release_state");
        out.push(b':');
        write_json_string(out, release.release_state.as_sql());
        out.push(b',');
        write_json_string(out, "revoked");
        out.push(b':');
        out.extend_from_slice(if release.revoked { b"true" } else { b"false" });
        out.push(b',');
        write_json_string(out, "source_id");
        out.push(b':');
        write_safe_dto_string(out, "source_id", &release.source_id);
        out.push(b',');
        write_json_string(out, "variant");
        out.push(b':');
        write_safe_dto_string(out, "variant", &release.variant);
        out.push(b',');
        write_json_string(out, "version");
        out.push(b':');
        write_safe_dto_string(out, "version", &release.version);
        out.push(b'}');
    }
    out.push(b']');
}

fn write_operations_json(out: &mut Vec<u8>, operations: &[StoredSourceOperation]) {
    out.push(b'[');
    for (index, operation) in operations.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        out.push(b'{');
        write_json_string(out, "created_at_ms");
        out.push(b':');
        out.extend_from_slice(operation.created_at_ms.to_string().as_bytes());
        out.push(b',');
        write_json_string(out, "operation_id");
        out.push(b':');
        write_safe_dto_string(out, "operation_id", &operation.operation_id);
        out.push(b',');
        write_json_string(out, "operation_kind");
        out.push(b':');
        write_json_string(out, operation.operation_kind.as_sql());
        out.push(b',');
        write_json_string(out, "operation_state");
        out.push(b':');
        write_json_string(out, operation.operation_state.as_sql());
        out.push(b',');
        write_json_string(out, "source_id");
        out.push(b':');
        write_safe_dto_string(out, "source_id", &operation.source_id);
        out.push(b',');
        write_json_string(out, "updated_at_ms");
        out.push(b':');
        out.extend_from_slice(operation.updated_at_ms.to_string().as_bytes());
        out.push(b'}');
    }
    out.push(b']');
}

fn take_required(
    object: &mut std::collections::BTreeMap<String, AstValue>,
    field: &str,
) -> Result<AstValue> {
    object
        .remove(field)
        .ok_or_else(|| anyhow!("Missing required field `{field}`"))
}

fn reject_unknown_fields(
    object: std::collections::BTreeMap<String, AstValue>,
    context: &str,
) -> Result<()> {
    if let Some((field, _)) = object.into_iter().next() {
        bail!("Unknown field `{field}` in {context}");
    }
    Ok(())
}

fn take_object(
    value: AstValue,
    context: &str,
) -> Result<std::collections::BTreeMap<String, AstValue>> {
    match value {
        AstValue::Object(value) => Ok(value),
        _ => bail!("{context} must be a JSON object"),
    }
}

fn take_array(value: AstValue, context: &str) -> Result<Vec<AstValue>> {
    match value {
        AstValue::Array(value) => Ok(value),
        _ => bail!("{context} must be a JSON array"),
    }
}

fn take_string(value: AstValue, field: &str) -> Result<String> {
    match value {
        AstValue::String(value) => Ok(value),
        _ => bail!("{field} must be a JSON string"),
    }
}

fn take_safe_string(value: AstValue, field: &str) -> Result<String> {
    let value = take_string(value, field)?;
    validate_safe_dto_string(field, &value)?;
    Ok(value)
}

fn take_integer(value: AstValue) -> Result<i64> {
    match value {
        AstValue::Integer(value) => Ok(value),
        _ => bail!("Expected JSON integer"),
    }
}

fn take_timestamp_ms(value: AstValue, field: &str) -> Result<i64> {
    checked_timestamp_ms(take_integer(value)?, field)
}

fn take_usize_count(value: AstValue, field: &str) -> Result<usize> {
    checked_usize_count(take_integer(value)?, field)
}

fn take_bool(value: AstValue, field: &str) -> Result<bool> {
    match value {
        AstValue::Bool(value) => Ok(value),
        _ => bail!("{field} must be a JSON boolean"),
    }
}

fn checked_timestamp_ms(value: i64, field: &str) -> Result<i64> {
    let millis = u64::try_from(value).map_err(|_| anyhow!("{field} must be non-negative"))?;
    i64::try_from(millis).map_err(|_| anyhow!("{field} exceeds i64 range"))
}

fn checked_usize_count(value: i64, field: &str) -> Result<usize> {
    let count = u64::try_from(value).map_err(|_| anyhow!("{field} must be non-negative"))?;
    usize::try_from(count).map_err(|_| anyhow!("{field} exceeds usize range"))
}

fn write_safe_dto_string(out: &mut Vec<u8>, field: &str, value: &str) {
    let safe = if validate_safe_dto_string(field, value).is_ok() {
        value
    } else {
        SAFE_DTO_REDACTION
    };
    write_json_string(out, safe);
}

fn validate_safe_dto_string(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{field} must not be empty");
    }
    if value.len() > 256 {
        bail!("{field} exceeds size limit");
    }
    if !value.is_ascii() || value.chars().any(|ch| ch.is_control()) {
        bail!("{field} contains unsupported characters");
    }
    if value.starts_with(' ') || value.ends_with(' ') {
        bail!("{field} must not have leading or trailing whitespace");
    }
    if value.contains("://")
        || value.to_ascii_lowercase().starts_with("http:")
        || value.to_ascii_lowercase().starts_with("https:")
        || value.to_ascii_lowercase().starts_with("file:")
        || value.to_ascii_lowercase().starts_with("urn:")
        || value.to_ascii_lowercase().contains("www.")
    {
        bail!("{field} must not contain a URL or URI");
    }
    if value
        .chars()
        .any(|ch| !ch.is_ascii_alphanumeric() && !matches!(ch, ' ' | '-' | '_' | '.' | '/'))
    {
        bail!("{field} contains unsupported punctuation");
    }
    let normalized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>();
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    if tokens
        .windows(2)
        .any(|window| window[0] == "internal" && window[1] == "code")
    {
        bail!("{field} contains forbidden sensitive language");
    }
    if tokens
        .iter()
        .any(|token| SAFE_DTO_FORBIDDEN_TOKENS.contains(token))
    {
        bail!("{field} contains a forbidden token");
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

impl SourceState {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Suspended => "suspended",
            Self::Revoked => "revoked",
        }
    }

    fn from_sql(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "suspended" => Ok(Self::Suspended),
            "revoked" => Ok(Self::Revoked),
            _ => bail!("Unknown source state"),
        }
    }
}

impl ReleaseState {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
            Self::Superseded => "superseded",
        }
    }

    fn from_sql(value: &str) -> Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "revoked" => Ok(Self::Revoked),
            "superseded" => Ok(Self::Superseded),
            _ => bail!("Unknown release state"),
        }
    }
}

impl SourceCatalogOperation {
    fn as_sql(self) -> &'static str {
        match self {
            Self::IngestSnapshot => "ingest_snapshot",
            Self::VerifySnapshot => "verify_snapshot",
            Self::ActivateSource => "activate_source",
            Self::RevokeRelease => "revoke_release",
        }
    }

    fn from_sql(value: &str) -> Result<Self> {
        match value {
            "ingest_snapshot" => Ok(Self::IngestSnapshot),
            "verify_snapshot" => Ok(Self::VerifySnapshot),
            "activate_source" => Ok(Self::ActivateSource),
            "revoke_release" => Ok(Self::RevokeRelease),
            _ => bail!("Unknown source operation kind"),
        }
    }
}

impl OperationState {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }

    fn from_sql(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "applied" => Ok(Self::Applied),
            "failed" => Ok(Self::Failed),
            "stopped" => Ok(Self::Stopped),
            _ => bail!("Unknown operation state"),
        }
    }
}
