mod intake_repository;
mod integrity;
mod lifecycle_repository;
mod managed_enrollment_repository;
mod migrations;
mod profile_repository;
mod records;
mod task_repository;

use std::fmt;
use std::fs::{File, OpenOptions};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use fs2::FileExt;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Pool, Row, Sqlite, Transaction};
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::config::paths::Paths;
use crate::mcp_platform::domain::ManagedMcpState;
use crate::mcp_platform::error::{McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::manifest::{
    parse_manifest, ManifestProof, ManifestSourceMetadata, OriginProvenance, SourceImportKind,
    SourceRef, UpdateChannel, VerifiedSourceDocumentRef,
};
use crate::mcp_platform::repository::{
    ApplyGovernedSourceRefresh, AuthorizationIssuer, ConfirmSourceProvisioning,
    ConnectionProjectionRecord, GovernedCatalogDocumentKind, GovernedCatalogDocumentRecord,
    GovernedCatalogEntryRecord, GovernedCatalogSourceRecord, GovernedSourceRefreshAuditRecord,
    GovernedSourceRefreshCommitOutcome, GovernedSourceRefreshRegistrationRecord,
    GovernedSourceRefreshResultState, GovernedSourceRefreshTransportKind,
    GovernedSourceRefreshTrustAnchorRecord, GovernedSourceTrustBasis, GovernedSourceTrustPinRecord,
    HttpsManifestProvisionRecord, InsertOutcome, ManagedMcpRecord, ManagedMcpStateUpdate,
    ManagedVersionRecord, ManifestRecord, ManifestSourceContext, NewManagedMcp, PlanRecord,
    PlanTarget, ProjectionAuthorityAnchor, RecordGovernedSourceRefreshAudit, RecoveryEligibility,
    SaveGovernedCatalogImport, SaveGovernedSourceRefreshRegistration, SavePlan,
    SourceProvisioningAuditRecord,
};
use records::{
    decode, decode_database_enum, decode_managed_row, decode_managed_version_row,
    decode_manifest_row, decode_plan_row, encode, encode_optional, error, fetch_valid_plan,
    find_managed_by_identity, integrity_error, managed_projection_integrity_payload, map_sqlx,
    not_found, plan_envelope_digest_for_save, repository_unavailable, revision_conflict,
    schema_objects, validate_manifest_proof, validate_plan_evidence, validate_plan_manifest,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseDiagnostics {
    pub foreign_keys: bool,
    pub journal_mode: String,
    pub busy_timeout_ms: i64,
    pub tables: Vec<String>,
    pub indexes: Vec<String>,
}

pub(crate) use integrity::system_signer as system_integrity_signer;
pub(crate) use integrity::{AnchorCheckpoint, InMemoryIntegritySigner, IntegritySigner};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredSourceCatalogDocumentProvenance {
    pub(crate) source_name: String,
    pub(crate) document_digest: String,
    pub(crate) canonical_digest: String,
    pub(crate) signed_digest: String,
    pub(crate) binding_digest: String,
    pub(crate) signature_kids: Vec<String>,
}

fn governed_document_kind_sql(kind: &GovernedCatalogDocumentKind) -> &'static str {
    match kind {
        GovernedCatalogDocumentKind::Manifest => "manifest",
        GovernedCatalogDocumentKind::Directory => "directory",
    }
}

fn parse_governed_document_kind(value: &str) -> McpPlatformResult<GovernedCatalogDocumentKind> {
    match value {
        "manifest" => Ok(GovernedCatalogDocumentKind::Manifest),
        "directory" => Ok(GovernedCatalogDocumentKind::Directory),
        _ => Err(integrity_error()),
    }
}

async fn finish_verified_read_snapshot<T>(
    transaction: Transaction<'_, Sqlite>,
    result: McpPlatformResult<T>,
) -> McpPlatformResult<T> {
    let rollback = transaction.rollback().await.map_err(map_sqlx);
    match (result, rollback) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

fn parse_governed_source_refresh_transport_kind(
    value: &str,
) -> McpPlatformResult<GovernedSourceRefreshTransportKind> {
    match value {
        "verified_source_bundle_v1" => {
            Ok(GovernedSourceRefreshTransportKind::VerifiedSourceBundleV1)
        }
        _ => Err(integrity_error()),
    }
}

fn parse_governed_source_refresh_result_state(
    value: &str,
) -> McpPlatformResult<GovernedSourceRefreshResultState> {
    match value {
        "idle" => Ok(GovernedSourceRefreshResultState::Idle),
        "succeeded" => Ok(GovernedSourceRefreshResultState::Succeeded),
        "failed" => Ok(GovernedSourceRefreshResultState::Failed),
        _ => Err(integrity_error()),
    }
}

fn parse_governed_source_trust_basis(value: &str) -> McpPlatformResult<GovernedSourceTrustBasis> {
    match value {
        "user_pin" => Ok(GovernedSourceTrustBasis::UserPin),
        _ => Err(integrity_error()),
    }
}

fn persisted_source_id(source_metadata: &ManifestSourceMetadata) -> McpPlatformResult<String> {
    match &source_metadata.source_ref {
        crate::mcp_platform::manifest::SourceRef::LocalPersistence => {
            Ok("local_persistence".to_string())
        }
        crate::mcp_platform::manifest::SourceRef::HttpsManifestUrl { manifest_url } => Ok(format!(
            "https_manifest_url_{}",
            crate::utils::bytes_to_hex(Sha256::digest(manifest_url.as_bytes()))
        )),
        crate::mcp_platform::manifest::SourceRef::VerifiedSourceCatalog { source_id } => {
            Ok(format!("verified_source_catalog_{source_id}"))
        }
        crate::mcp_platform::manifest::SourceRef::EnterpriseDirectory { .. } => Err(error(
            McpPlatformErrorCode::NotImplementedForPhase,
            "enterprise directory source context is not implemented for this phase",
        )),
    }
}

const GOVERNED_CATALOG_ENTRY_PROJECTION: &str = r#"SELECT
        s.source_id, s.import_kind, s.source_ref_json, s.display_name,
        s.created_at_ms AS source_created_at_ms, s.updated_at_ms AS source_updated_at_ms,
        d.document_id, d.document_digest, d.document_kind, d.created_at_ms AS document_created_at_ms,
        trusted.document_digest AS trusted_document_digest,
        trusted.canonical_digest AS trusted_canonical_digest,
        trusted.signed_digest AS trusted_signed_digest,
        trusted.binding_digest AS trusted_binding_digest,
        (SELECT group_concat(kid, char(31)) FROM (SELECT kid FROM source_signatures WHERE source_id = trusted.source_id ORDER BY signature_order)) AS trusted_signature_kids,
        r.release_id AS trusted_release_id, r.mcp_id AS trusted_release_mcp_id,
        r.version AS trusted_release_version,
        r.release_status AS trusted_release_status,
        e.entry_id, e.proof_json, e.trust_tier_json, e.source_metadata_json, e.created_at_ms,
        m.manifest_digest, m.mcp_id, m.version, m.canonical_bytes
   FROM governed_catalog_entries e
   JOIN governed_catalog_sources s ON s.source_id = e.source_id
   JOIN governed_catalog_documents d ON d.source_id = e.source_id AND d.document_id = e.document_id
   JOIN source_catalog_documents trusted
     ON trusted.source_id = json_extract(s.source_ref_json, '$.source_id')
    AND trusted.document_digest = d.document_digest
   JOIN manifest_blobs m
     ON m.manifest_digest = e.manifest_digest
    AND m.mcp_id = e.mcp_id
    AND m.version = e.version
   LEFT JOIN source_releases r
     ON r.source_id = trusted.source_id
    AND r.release_id = e.entry_id
    AND r.mcp_id = m.mcp_id
    AND r.version = m.version"#;

async fn save_manifest_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    record: &ManifestRecord,
) -> McpPlatformResult<InsertOutcome> {
    record
        .source_metadata
        .validate_trust_tier(record.trust_tier)?;
    if let Some(row) = sqlx::query(
        r#"SELECT manifest_digest, mcp_id, version, canonical_bytes, proof_json,
            trust_tier_json, source_ref_json, import_kind, release_id,
            origin_provenance_json, update_channel_json, created_at_ms FROM manifest_blobs
            WHERE mcp_id = ? AND version = ?"#,
    )
    .bind(&record.verified.manifest().id)
    .bind(record.verified.manifest().version.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx)?
    {
        let existing = decode_manifest_row(&row)?;
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
            source_ref_json, import_kind, release_id, origin_provenance_json,
            update_channel_json, created_at_ms
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(record.verified.digest())
    .bind(&record.verified.manifest().id)
    .bind(record.verified.manifest().version.as_str())
    .bind(record.verified.canonical_json())
    .bind(encode(&record.proof)?)
    .bind(encode(&record.trust_tier)?)
    .bind(encode(&record.source_metadata.source_ref)?)
    .bind(record.source_metadata.import_kind.as_str())
    .bind(&record.source_metadata.release_id)
    .bind(encode(&record.source_metadata.origin_provenance)?)
    .bind(encode(&record.source_metadata.update_channel)?)
    .bind(record.created_at_ms)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    Ok(InsertOutcome::Inserted)
}

fn decode_governed_catalog_entry_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<GovernedCatalogEntryRecord> {
    let manifest_digest: String = row.try_get("manifest_digest").map_err(map_sqlx)?;
    let canonical_bytes: Vec<u8> = row.try_get("canonical_bytes").map_err(map_sqlx)?;
    let verified = parse_manifest(&canonical_bytes)?;
    if verified.digest() != manifest_digest {
        return Err(integrity_error());
    }
    let proof: ManifestProof = decode(row.try_get::<&str, _>("proof_json").map_err(map_sqlx)?)?;
    validate_manifest_proof(&proof, verified.digest())?;
    let trust_tier = decode(
        row.try_get::<&str, _>("trust_tier_json")
            .map_err(map_sqlx)?,
    )?;
    let source_metadata: ManifestSourceMetadata = decode(
        row.try_get::<&str, _>("source_metadata_json")
            .map_err(map_sqlx)?,
    )?;
    source_metadata.validate_trust_tier(trust_tier)?;
    let source_import_kind = decode_database_enum::<SourceImportKind>(
        row.try_get::<&str, _>("import_kind").map_err(map_sqlx)?,
    )?;
    let source_ref: SourceRef = decode(
        row.try_get::<&str, _>("source_ref_json")
            .map_err(map_sqlx)?,
    )?;
    let source_id: String = row.try_get("source_id").map_err(map_sqlx)?;
    validate_governed_source_identity(&source_id, source_import_kind, &source_ref)?;
    let source = GovernedCatalogSourceRecord {
        source_id,
        import_kind: source_import_kind,
        source_ref,
        display_name: row.try_get("display_name").map_err(map_sqlx)?,
        created_at_ms: row.try_get("source_created_at_ms").map_err(map_sqlx)?,
        updated_at_ms: row.try_get("source_updated_at_ms").map_err(map_sqlx)?,
    };
    let document = GovernedCatalogDocumentRecord {
        source_id: source.source_id.clone(),
        document_id: row.try_get("document_id").map_err(map_sqlx)?,
        document_digest: row.try_get("document_digest").map_err(map_sqlx)?,
        document_kind: parse_governed_document_kind(
            row.try_get::<&str, _>("document_kind").map_err(map_sqlx)?,
        )?,
        created_at_ms: row.try_get("document_created_at_ms").map_err(map_sqlx)?,
    };
    if document.source_id != source.source_id {
        return Err(integrity_error());
    }
    let SourceRef::VerifiedSourceCatalog {
        source_id: source_ref_id,
    } = &source.source_ref
    else {
        return Err(integrity_error());
    };
    let Some(provenance) = source_metadata
        .origin_provenance
        .verified_source_document
        .as_ref()
    else {
        return Err(integrity_error());
    };
    if source_metadata.source_ref != source.source_ref
        || source_metadata.import_kind != source.import_kind
        || provenance.source_id != *source_ref_id
        || provenance.document_digest != document.document_digest
        || document.document_digest
            != row
                .try_get::<String, _>("trusted_document_digest")
                .map_err(map_sqlx)?
        || provenance.canonical_digest
            != row
                .try_get::<String, _>("trusted_canonical_digest")
                .map_err(map_sqlx)?
        || provenance.signed_digest
            != row
                .try_get::<String, _>("trusted_signed_digest")
                .map_err(map_sqlx)?
        || provenance.binding_digest
            != row
                .try_get::<String, _>("trusted_binding_digest")
                .map_err(map_sqlx)?
        || provenance.signature_kids
            != row
                .try_get::<Option<String>, _>("trusted_signature_kids")
                .map_err(map_sqlx)?
                .map(|value| {
                    value
                        .split('\u{1f}')
                        .map(str::to_string)
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default()
        || trust_tier != crate::mcp_platform::TrustTier::Official
    {
        return Err(integrity_error());
    }
    if !matches!(&proof, ManifestProof::Catalog { index_digest, .. } if index_digest == &document.document_digest)
    {
        return Err(integrity_error());
    }
    let entry_id: String = row.try_get("entry_id").map_err(map_sqlx)?;
    let Some(release_id) = source_metadata.release_id.as_deref() else {
        return Err(integrity_error());
    };
    if release_id != entry_id {
        return Err(integrity_error());
    }
    if !matches!(&proof, ManifestProof::Catalog {
        declared_manifest_digest,
        index_digest,
        signature: None,
    } if declared_manifest_digest == &manifest_digest && index_digest == &document.document_digest)
    {
        return Err(integrity_error());
    }
    let trusted_release_id: Option<String> = row.try_get("trusted_release_id").map_err(map_sqlx)?;
    if trusted_release_id.as_deref() != Some(entry_id.as_str())
        || row
            .try_get::<Option<String>, _>("trusted_release_mcp_id")
            .map_err(map_sqlx)?
            .as_deref()
            != Some(verified.manifest().id.as_str())
        || row
            .try_get::<Option<String>, _>("trusted_release_version")
            .map_err(map_sqlx)?
            .as_deref()
            != Some(verified.manifest().version.as_str())
        || row
            .try_get::<Option<String>, _>("trusted_release_status")
            .map_err(map_sqlx)?
            .as_deref()
            != Some("active")
    {
        return Err(integrity_error());
    }
    Ok(GovernedCatalogEntryRecord {
        source,
        document,
        entry_id,
        manifest: ManifestRecord {
            verified,
            proof,
            trust_tier,
            source_metadata,
            created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
        },
    })
}

fn decode_governed_catalog_source_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<GovernedCatalogSourceRecord> {
    let import_kind = decode_database_enum::<SourceImportKind>(
        row.try_get::<&str, _>("import_kind").map_err(map_sqlx)?,
    )?;
    let source_id: String = row.try_get("source_id").map_err(map_sqlx)?;
    let source_ref = decode(
        row.try_get::<&str, _>("source_ref_json")
            .map_err(map_sqlx)?,
    )?;
    validate_governed_source_identity(&source_id, import_kind, &source_ref)?;
    Ok(GovernedCatalogSourceRecord {
        source_id,
        import_kind,
        source_ref,
        display_name: row.try_get("display_name").map_err(map_sqlx)?,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
        updated_at_ms: row.try_get("updated_at_ms").map_err(map_sqlx)?,
    })
}

fn validate_governed_source_identity(
    source_id: &str,
    import_kind: SourceImportKind,
    source_ref: &SourceRef,
) -> McpPlatformResult<()> {
    let SourceRef::VerifiedSourceCatalog {
        source_id: source_ref_id,
    } = source_ref
    else {
        return Err(integrity_error());
    };
    if source_id != format!("verified_source_catalog_{source_ref_id}")
        || import_kind != source_ref.import_kind()
    {
        return Err(integrity_error());
    }
    Ok(())
}

fn decode_governed_source_refresh_registration_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<GovernedSourceRefreshRegistrationRecord> {
    Ok(GovernedSourceRefreshRegistrationRecord {
        source_id: row.try_get("source_id").map_err(map_sqlx)?,
        transport_kind: parse_governed_source_refresh_transport_kind(
            row.try_get::<&str, _>("transport_kind").map_err(map_sqlx)?,
        )?,
        endpoint: row.try_get("endpoint").map_err(map_sqlx)?,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
        updated_at_ms: row.try_get("updated_at_ms").map_err(map_sqlx)?,
        last_attempted_at_ms: row.try_get("last_attempted_at_ms").map_err(map_sqlx)?,
        last_refreshed_at_ms: row.try_get("last_refreshed_at_ms").map_err(map_sqlx)?,
        last_result: parse_governed_source_refresh_result_state(
            row.try_get::<&str, _>("last_result").map_err(map_sqlx)?,
        )?,
        last_document_digest: row.try_get("last_document_digest").map_err(map_sqlx)?,
        last_error_code: row.try_get("last_error_code").map_err(map_sqlx)?,
    })
}

fn decode_governed_source_refresh_trust_anchor_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<GovernedSourceRefreshTrustAnchorRecord> {
    let endpoint: String = row.try_get("registration_endpoint").map_err(map_sqlx)?;
    let anchor_endpoint: String = row.try_get("anchor_endpoint").map_err(map_sqlx)?;
    if endpoint != anchor_endpoint {
        return Err(integrity_error());
    }
    let verified_source_id: String = row.try_get("verified_source_id").map_err(map_sqlx)?;
    let root_digest: String = row.try_get("root_digest").map_err(map_sqlx)?;
    let anchor_document_digest: String = row.try_get("anchor_document_digest").map_err(map_sqlx)?;
    let anchor_document_bytes: Vec<u8> = row.try_get("anchor_document_bytes").map_err(map_sqlx)?;
    let document = crate::verified_source_catalog::parse_signed_envelope(&anchor_document_bytes)
        .map_err(|_| integrity_error())?;
    let anchor = document.trust_anchor();
    if anchor.source_id != verified_source_id
        || anchor.root_digest != root_digest
        || document.digests.document_digest != anchor_document_digest
    {
        return Err(integrity_error());
    }
    Ok(GovernedSourceRefreshTrustAnchorRecord {
        source_id: row.try_get("source_id").map_err(map_sqlx)?,
        endpoint,
        anchor,
        anchor_document_digest,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
    })
}

fn decode_governed_source_trust_pin_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<GovernedSourceTrustPinRecord> {
    Ok(GovernedSourceTrustPinRecord {
        source_id: row.try_get("source_id").map_err(map_sqlx)?,
        verified_source_id: row.try_get("verified_source_id").map_err(map_sqlx)?,
        trust_basis: parse_governed_source_trust_basis(
            row.try_get::<&str, _>("trust_basis").map_err(map_sqlx)?,
        )?,
        root_digest: row.try_get("root_digest").map_err(map_sqlx)?,
        endpoint: row.try_get("endpoint").map_err(map_sqlx)?,
        source_document_digest: row.try_get("source_document_digest").map_err(map_sqlx)?,
        descriptor_digest: row.try_get("descriptor_digest").map_err(map_sqlx)?,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
    })
}

fn decode_governed_source_refresh_audit_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<GovernedSourceRefreshAuditRecord> {
    Ok(GovernedSourceRefreshAuditRecord {
        audit_id: row.try_get("audit_id").map_err(map_sqlx)?,
        source_id: row.try_get("source_id").map_err(map_sqlx)?,
        document_digest: row.try_get("document_digest").map_err(map_sqlx)?,
        result: parse_governed_source_refresh_result_state(
            row.try_get::<&str, _>("result").map_err(map_sqlx)?,
        )?,
        error_code: row.try_get("error_code").map_err(map_sqlx)?,
        actor: row.try_get("actor").map_err(map_sqlx)?,
        correlation_id: row.try_get("correlation_id").map_err(map_sqlx)?,
        occurred_at_ms: row.try_get("occurred_at_ms").map_err(map_sqlx)?,
    })
}

struct PathBoundTestIntegritySigner {
    inner: Arc<dyn IntegritySigner>,
    identity: integrity::AnchorIdentity,
    checkpoint_state: Arc<PathBoundTestSignerState>,
}

struct PathBoundTestSignerState {
    checkpoint: std::sync::Mutex<Option<integrity::AnchorCheckpoint>>,
}

struct PathBoundTestSignerRegistryEntry {
    checkpoint_state: Weak<PathBoundTestSignerState>,
}

fn path_bound_test_signer_registry() -> &'static std::sync::Mutex<
    std::collections::HashMap<(usize, String), PathBoundTestSignerRegistryEntry>,
> {
    static REGISTRY: OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<(usize, String), PathBoundTestSignerRegistryEntry>,
        >,
    > = OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn path_bound_test_signer_registry_key(signer: &Arc<dyn IntegritySigner>) -> usize {
    Arc::as_ptr(signer).cast::<()>() as usize
}

fn path_bound_test_signer_registry_cleanup(
    registry: &mut std::collections::HashMap<(usize, String), PathBoundTestSignerRegistryEntry>,
) {
    registry.retain(|_, entry| entry.checkpoint_state.upgrade().is_some());
}

fn path_bound_test_signer_checkpoint_state(
    signer: &Arc<dyn IntegritySigner>,
    path_binding: &str,
) -> McpPlatformResult<Arc<PathBoundTestSignerState>> {
    let mut registry = path_bound_test_signer_registry()
        .lock()
        .map_err(|_| path_bound_test_signer_lock_error())?;
    path_bound_test_signer_registry_cleanup(&mut registry);
    let key = (
        path_bound_test_signer_registry_key(signer),
        path_binding.to_string(),
    );
    if let Some(entry) = registry.get(&key) {
        if let Some(state) = entry.checkpoint_state.upgrade() {
            return Ok(state);
        }
    }
    let state = Arc::new(PathBoundTestSignerState {
        checkpoint: std::sync::Mutex::new(None),
    });
    registry.insert(
        key,
        PathBoundTestSignerRegistryEntry {
            checkpoint_state: Arc::downgrade(&state),
        },
    );
    Ok(state)
}

#[cfg(test)]
fn path_bound_test_signer_registry_contains_for_testing(
    signer_key: usize,
    path_binding: &str,
) -> bool {
    let mut registry = path_bound_test_signer_registry().lock().unwrap();
    path_bound_test_signer_registry_cleanup(&mut registry);
    registry.contains_key(&(signer_key, path_binding.to_string()))
}

#[cfg(test)]
fn path_bound_test_signer_registry_entry_count_for_testing(signer_key: usize) -> usize {
    let mut registry = path_bound_test_signer_registry().lock().unwrap();
    path_bound_test_signer_registry_cleanup(&mut registry);
    registry
        .keys()
        .filter(|(entry_signer_key, _)| *entry_signer_key == signer_key)
        .count()
}

fn path_bound_test_signer_lock_error() -> crate::mcp_platform::error::McpPlatformError {
    error(
        McpPlatformErrorCode::IntegrityUnavailable,
        "managed MCP integrity anchor is unavailable",
    )
}

fn path_bound_test_signer_publish_is_monotonic(
    expected: Option<&integrity::AnchorCheckpoint>,
    next: &integrity::AnchorCheckpoint,
    identity: &integrity::AnchorIdentity,
) -> bool {
    if next.identity != *identity {
        return false;
    }
    match expected {
        Some(current) => next.sequence == current.sequence + 1 && next.root != current.root,
        None => next.sequence == 0,
    }
}

impl PathBoundTestIntegritySigner {
    fn rebound(
        inner: Arc<dyn IntegritySigner>,
        path_binding: &str,
    ) -> McpPlatformResult<Arc<dyn IntegritySigner>> {
        let mut identity = inner.identity()?;
        identity.path_binding = path_binding.to_string();
        let checkpoint_state =
            path_bound_test_signer_checkpoint_state(&inner, &identity.path_binding)?;
        Ok(Arc::new(Self {
            checkpoint_state,
            inner,
            identity,
        }))
    }
}

impl integrity::AnchorProvider for PathBoundTestIntegritySigner {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }

    fn assurance(&self) -> integrity::AnchorProviderAssurance {
        self.inner.assurance()
    }

    fn health(&self) -> integrity::AnchorProviderHealth {
        self.inner.health()
    }

    fn identity(&self) -> McpPlatformResult<integrity::AnchorIdentity> {
        Ok(self.identity.clone())
    }

    fn checkpoint(&self) -> McpPlatformResult<Option<integrity::AnchorCheckpoint>> {
        let checkpoint = self
            .checkpoint_state
            .checkpoint
            .lock()
            .map_err(|_| path_bound_test_signer_lock_error())?;
        Ok(checkpoint.clone())
    }

    fn publish(
        &self,
        expected: Option<&integrity::AnchorCheckpoint>,
        next: &integrity::AnchorCheckpoint,
    ) -> McpPlatformResult<()> {
        let mut checkpoint = self
            .checkpoint_state
            .checkpoint
            .lock()
            .map_err(|_| path_bound_test_signer_lock_error())?;
        let current = checkpoint.clone();
        if current.as_ref() != expected
            || !path_bound_test_signer_publish_is_monotonic(expected, next, &self.identity)
        {
            return Err(integrity::integrity_recovery_required());
        }
        *checkpoint = Some(next.clone());
        Ok(())
    }

    fn sign(&self, domain: &str, canonical_payload: &[u8]) -> McpPlatformResult<String> {
        self.inner.sign(domain, canonical_payload)
    }

    fn matches_expected_path_binding(&self, expected: &str, actual: &str) -> bool {
        actual == expected
    }
}

fn normalize_file_backed_integrity_signer(
    signer: Arc<dyn IntegritySigner>,
    expected_path_binding: &str,
) -> McpPlatformResult<Arc<dyn IntegritySigner>> {
    if !matches!(
        signer.assurance(),
        integrity::AnchorProviderAssurance::TestInMemory
    ) {
        return Ok(signer);
    }
    let identity = signer.identity()?;
    if identity.path_binding == expected_path_binding
        || !integrity::matches_expected_path_binding(
            signer.as_ref(),
            expected_path_binding,
            &identity.path_binding,
        )
    {
        return Ok(signer);
    }
    PathBoundTestIntegritySigner::rebound(signer, expected_path_binding)
}

fn normalize_in_memory_integrity_signer(
    signer: Arc<dyn IntegritySigner>,
) -> McpPlatformResult<Arc<dyn IntegritySigner>> {
    if !matches!(
        signer.assurance(),
        integrity::AnchorProviderAssurance::TestInMemory
    ) {
        return Ok(signer);
    }
    let identity = signer.identity()?;
    if identity.path_binding.len() == 64 {
        return Ok(signer);
    }
    let digest = Sha256::digest(b"lumina.mcp-platform.in-memory-path-binding-v1");
    let binding = crate::utils::bytes_to_hex(digest);
    PathBoundTestIntegritySigner::rebound(signer, &binding)
}

#[derive(Clone)]
pub struct SqliteMcpPlatformRepository {
    pool: Pool<Sqlite>,
    database_path: Option<PathBuf>,
    database_lock_path: Option<PathBuf>,
    integrity_signer: Arc<dyn IntegritySigner>,
    integrity_transaction_lock: Arc<Mutex<()>>,
    authorization_issuer: Arc<AuthorizationIssuer>,
}

struct LockedDatabaseTarget {
    database_path: PathBuf,
    lock_path: PathBuf,
    path_binding: String,
    existed_before_open: bool,
}

impl fmt::Debug for SqliteMcpPlatformRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SqliteMcpPlatformRepository")
            .field("database_path", &self.database_path)
            .finish_non_exhaustive()
    }
}

impl SqliteMcpPlatformRepository {
    pub(crate) fn projection_writer_repository_identity(
        &self,
    ) -> McpPlatformResult<ProjectionAuthorityAnchor> {
        let identity = self.integrity_signer.identity()?;
        Ok(ProjectionAuthorityAnchor {
            instance_id: identity.instance_id,
            path_binding: identity.path_binding,
            key_epoch: identity.key_epoch,
            sequence: 1,
            root: String::new(),
        })
    }

    pub(crate) fn sign_projection_receipt_binding(
        &self,
        domain: &str,
        payload: &[u8],
    ) -> McpPlatformResult<String> {
        self.integrity_signer.sign(domain, payload)
    }

    pub async fn open_default() -> McpPlatformResult<Self> {
        Self::open_path(Paths::in_data_dir("mcp-platform/platform.db")).await
    }

    pub(crate) async fn trusted_reenroll_path(
        path: impl AsRef<Path>,
    ) -> McpPlatformResult<(Self, PathBuf)> {
        let path = path.as_ref();
        let expected_path_binding = database_path_binding(path)?;
        let lock_path = integrity_lock_path(path);
        let mut database_lock = DatabaseLockGuard::acquire(&lock_path)?;
        let locked_target = match locked_database_target(
            path,
            database_lock.resolved_path(),
            expected_path_binding.as_str(),
        ) {
            Ok(target) => target,
            Err(error) => return fail_after_cleanup(error, [database_lock.cleanup()]),
        };
        if !locked_target.existed_before_open || !locked_target.database_path.is_file() {
            database_lock.cleanup()?;
            return Err(error(
                McpPlatformErrorCode::InvalidRequest,
                "trusted re-enrollment requires an existing database",
            ));
        }
        let quarantine = trusted_reenrollment_quarantine_path(&locked_target.database_path);
        let source_paths = sqlite_database_and_sidecar_paths(&locked_target.database_path);
        let quarantine_paths = sqlite_database_and_sidecar_paths(&quarantine);
        let mut moved_paths = Vec::new();
        let mut path_pairs = source_paths.iter().zip(quarantine_paths.iter());
        let (database_source, database_destination) = path_pairs
            .next()
            .expect("sqlite reenrollment path list includes the database");
        if let Err(error) = rename_for_trusted_reenrollment(
            database_source,
            database_destination,
            TrustedReenrollmentRenamePhase::Move,
        ) {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                return fail_closed_after_anchor_reset(
                    repository_unavailable(),
                    &locked_target.path_binding,
                    [database_lock.cleanup()],
                );
            }
            return fail_after_cleanup(repository_unavailable(), [database_lock.cleanup()]);
        }
        moved_paths.push((database_destination.clone(), database_source.clone()));
        for (source, destination) in path_pairs {
            if !source.exists() {
                continue;
            }
            if let Err(error) = rename_for_trusted_reenrollment(
                source,
                destination,
                TrustedReenrollmentRenamePhase::Move,
            ) {
                if restore_moved_paths(&moved_paths).is_err() {
                    return fail_after_cleanup(
                        trusted_reenrollment_restore_failed(),
                        [database_lock.cleanup()],
                    );
                }
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    return fail_closed_after_anchor_reset(
                        repository_unavailable(),
                        &locked_target.path_binding,
                        [database_lock.cleanup()],
                    );
                }
                return fail_after_cleanup(repository_unavailable(), [database_lock.cleanup()]);
            }
            moved_paths.push((destination.clone(), source.clone()));
        }
        if let Err(error) =
            integrity::clear_system_anchor_for_trusted_reenrollment(&locked_target.path_binding)
        {
            if restore_moved_paths(&moved_paths).is_err() {
                return fail_after_cleanup(
                    trusted_reenrollment_restore_failed(),
                    [database_lock.cleanup()],
                );
            }
            return fail_after_cleanup(error, [database_lock.cleanup()]);
        }
        #[cfg(test)]
        run_after_trusted_reenrollment_anchor_clear_hook(&locked_target.database_path);
        let repository = match Self::open_bootstrapped_file_backed_with_locked_target(
            &locked_target,
            SqliteConnectOptions::new()
                .filename(&locked_target.database_path)
                .create_if_missing(true)
                .foreign_keys(true)
                .busy_timeout(Duration::from_secs(30))
                .journal_mode(SqliteJournalMode::Wal),
            5,
            integrity::system_signer(
                &locked_target.path_binding,
                !locked_target.database_path.exists(),
            ),
        )
        .await
        {
            Ok(repository) => {
                database_lock.disarm_cleanup();
                repository
            }
            Err(failure) => {
                let rollback =
                    rollback_trusted_reenrollment_bootstrap_failure(&failure, &moved_paths, || {
                        integrity::clear_system_anchor_for_trusted_reenrollment(
                            &locked_target.path_binding,
                        )
                    });
                let error = match rollback {
                    Ok(()) => failure.into_error(),
                    Err(rollback_error) => rollback_error,
                };
                return fail_after_cleanup(error, [database_lock.cleanup()]);
            }
        };
        database_lock.cleanup()?;
        Ok((repository, quarantine))
    }

    pub async fn open_path(path: impl AsRef<Path>) -> McpPlatformResult<Self> {
        let path = path.as_ref();
        let allow_key_creation = !path.exists();
        let path_binding = database_path_binding(path)?;
        let integrity_signer = integrity::system_signer(&path_binding, allow_key_creation);
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(30))
            .journal_mode(SqliteJournalMode::Wal);
        Self::open_file_backed_with_options(options, 5, integrity_signer, path, path_binding).await
    }

    pub(crate) async fn open_path_with_integrity_signer(
        path: impl AsRef<Path>,
        integrity_signer: Arc<dyn IntegritySigner>,
    ) -> McpPlatformResult<Self> {
        let path = path.as_ref();
        let path_binding = database_path_binding(path)?;
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(30))
            .journal_mode(SqliteJournalMode::Wal);
        Self::open_file_backed_with_options(options, 5, integrity_signer, path, path_binding).await
    }

    #[cfg(feature = "integration-test-support")]
    pub async fn open_path_for_integration_test(path: impl AsRef<Path>) -> McpPlatformResult<Self> {
        let path = path.as_ref();
        let path_binding = database_path_binding(path)?;
        let integrity_signer =
            InMemoryIntegritySigner::new_for_testing_with_path_binding([0x49; 32], path_binding);
        Self::open_path_with_integrity_signer(path, integrity_signer).await
    }

    pub async fn open_url(url: &str) -> McpPlatformResult<Self> {
        let _ = SqliteConnectOptions::from_str(url).map_err(|_| repository_unavailable())?;
        let signer = integrity::unavailable_signer();
        Err(integrity::preflight_open_error(signer.as_ref())
            .expect("pathless production URL repositories must fail closed"))
    }

    pub(crate) async fn open_url_with_integrity_signer(
        url: &str,
        integrity_signer: Arc<dyn IntegritySigner>,
    ) -> McpPlatformResult<Self> {
        let options = SqliteConnectOptions::from_str(url)
            .map_err(|_| repository_unavailable())?
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(30))
            .journal_mode(SqliteJournalMode::Wal);
        let in_memory = url_is_in_memory(url);
        let max_connections = if in_memory { 1 } else { 5 };
        if in_memory {
            let integrity_signer = normalize_in_memory_integrity_signer(integrity_signer)?;
            preflight_new_repository_open(integrity_signer.as_ref(), None)?;
            return Self::open_with_options(
                options,
                None,
                max_connections,
                integrity_signer,
                true,
                None,
                None,
            )
            .await;
        }
        let database_path = options.get_filename().to_path_buf();
        let path_binding = database_path_binding(&database_path)?;
        Self::open_file_backed_with_options(
            options,
            max_connections,
            integrity_signer,
            &database_path,
            path_binding,
        )
        .await
    }

    async fn open_file_backed_with_options(
        options: SqliteConnectOptions,
        max_connections: u32,
        integrity_signer: Arc<dyn IntegritySigner>,
        path: &Path,
        path_binding: String,
    ) -> McpPlatformResult<Self> {
        let integrity_signer =
            normalize_file_backed_integrity_signer(integrity_signer, &path_binding)?;
        let expected_existing_repository = path.exists();
        if expected_existing_repository {
            preflight_existing_repository_open(
                path,
                integrity_signer.as_ref(),
                Some(&path_binding),
            )
            .await?;
        } else {
            preflight_new_repository_open(integrity_signer.as_ref(), Some(&path_binding))?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|_| repository_unavailable())?;
            }
        }
        #[cfg(test)]
        run_after_preflight_before_lock_hook(path);
        let lock_path = integrity_lock_path(path);
        let mut initialization_lock = DatabaseLockGuard::acquire(&lock_path)?;
        let locked_target = match locked_database_target(
            path,
            initialization_lock.resolved_path(),
            path_binding.as_str(),
        ) {
            Ok(target) => target,
            Err(error) => return fail_after_cleanup(error, [initialization_lock.cleanup()]),
        };
        if locked_target.existed_before_open {
            // Re-verify under the lock so preflight cannot be raced by replacing the bound path.
            if let Err(error) = preflight_existing_repository_open(
                &locked_target.database_path,
                integrity_signer.as_ref(),
                Some(&locked_target.path_binding),
            )
            .await
            {
                return fail_after_cleanup(error, [initialization_lock.cleanup()]);
            }
            let repository = Self::open_with_options(
                options.clone().filename(&locked_target.database_path),
                Some(locked_target.database_path.clone()),
                max_connections,
                integrity_signer,
                false,
                Some(locked_target.path_binding),
                Some(locked_target.lock_path),
            )
            .await;
            if repository.is_ok() {
                initialization_lock.disarm_cleanup();
            } else {
                return fail_after_cleanup(
                    repository.expect_err("repository open failure is propagated"),
                    [initialization_lock.cleanup()],
                );
            }
            return repository;
        }

        if let Err(error) = preflight_new_repository_open(
            integrity_signer.as_ref(),
            Some(&locked_target.path_binding),
        ) {
            return fail_after_cleanup(error, [initialization_lock.cleanup()]);
        }
        let repository = match Self::open_bootstrapped_file_backed_with_locked_target(
            &locked_target,
            options,
            max_connections,
            integrity_signer,
        )
        .await
        {
            Ok(repository) => repository,
            Err(failure) => {
                let cleanup_targets = failure.cleanup_paths();
                let cleanup_error = failure.cleanup_error();
                let bootstrap_error = failure.into_error();
                return fail_after_cleanup(
                    bootstrap_error,
                    [
                        cleanup_paths(&cleanup_targets, cleanup_error),
                        initialization_lock.cleanup(),
                    ],
                );
            }
        };
        initialization_lock.disarm_cleanup();
        Ok(repository)
    }

    async fn open_bootstrapped_file_backed_with_locked_target(
        locked_target: &LockedDatabaseTarget,
        options: SqliteConnectOptions,
        max_connections: u32,
        integrity_signer: Arc<dyn IntegritySigner>,
    ) -> Result<Self, FileBackedBootstrapFailure> {
        // Bootstrap in a temp database bound to the authoritative physical path so publish
        // failure never exposes a partially initialized repository at the final location.
        let staged_path = staging_database_path(&locked_target.database_path);
        let staged_options = options
            .clone()
            .filename(&staged_path)
            .journal_mode(SqliteJournalMode::Delete);
        let staged_repository = Self::open_with_options(
            staged_options,
            Some(staged_path.clone()),
            1,
            integrity_signer.clone(),
            true,
            Some(locked_target.path_binding.clone()),
            None,
        )
        .await
        .map_err(|error| FileBackedBootstrapFailure::staged(error, staged_path.clone()))?;
        staged_repository.close().await;
        drop(staged_repository);
        activate_staged_database(&staged_path, &locked_target.database_path).map_err(
            |failure| {
                if failure.final_created {
                    FileBackedBootstrapFailure::created_final(
                        failure.error,
                        staged_path.clone(),
                        locked_target.database_path.clone(),
                    )
                } else {
                    FileBackedBootstrapFailure::staged(failure.error, staged_path.clone())
                }
            },
        )?;
        #[cfg(test)]
        run_after_activation_before_final_open_hook(&locked_target.database_path, &staged_path)
            .map_err(|error| {
                FileBackedBootstrapFailure::final_path(error, locked_target.database_path.clone())
            })?;
        Self::open_with_options(
            options.filename(&locked_target.database_path),
            Some(locked_target.database_path.clone()),
            max_connections,
            integrity_signer,
            false,
            Some(locked_target.path_binding.clone()),
            Some(locked_target.lock_path.clone()),
        )
        .await
        .map_err(|error| {
            FileBackedBootstrapFailure::final_path(error, locked_target.database_path.clone())
        })
    }

    async fn open_with_options(
        options: SqliteConnectOptions,
        database_path: Option<PathBuf>,
        max_connections: u32,
        integrity_signer: Arc<dyn IntegritySigner>,
        allow_bootstrap: bool,
        expected_path_binding: Option<String>,
        database_lock_path: Option<PathBuf>,
    ) -> McpPlatformResult<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .connect_with(options)
            .await
            .map_err(map_sqlx)?;
        if let Err(error) = preflight_migration_state(&pool, integrity_signer.as_ref()).await {
            pool.close().await;
            return Err(error);
        }
        let open_result = if matches!(
            integrity_signer.assurance(),
            integrity::AnchorProviderAssurance::SystemKeyring
        ) {
            match migrate_through_version(&pool, 14).await {
                Ok(schema_was_absent) => {
                    complete_system_keyring_schema_guarded_open(
                        &pool,
                        integrity_signer.as_ref(),
                        allow_bootstrap && schema_was_absent,
                        expected_path_binding.as_deref(),
                    )
                    .await
                }
                Err(error) => Err(error),
            }
        } else {
            match load_current_schema_version(&pool).await {
                Ok(current) => {
                    let migrate_result =
                        if current >= 15 && current < migrations::CURRENT_SCHEMA_VERSION {
                            migrate_through_version_with_integrity_commit(
                                &pool,
                                integrity_signer.as_ref(),
                                expected_path_binding.as_deref(),
                                migrations::CURRENT_SCHEMA_VERSION,
                            )
                            .await
                        } else {
                            migrate(&pool).await
                        };
                    match migrate_result {
                        Ok(schema_was_absent) => {
                            if let Err(error) = initialize_or_verify_integrity(
                                &pool,
                                integrity_signer.as_ref(),
                                allow_bootstrap && schema_was_absent,
                                expected_path_binding.as_deref(),
                            )
                            .await
                            {
                                Err(error)
                            } else {
                                ensure_schema_compatibility_fence(&pool, integrity_signer.as_ref())
                                    .await
                            }
                        }
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            }
        };
        if let Err(error) = open_result {
            pool.close().await;
            return Err(error);
        }
        Ok(Self {
            pool,
            database_path,
            database_lock_path,
            integrity_signer,
            integrity_transaction_lock: Arc::new(Mutex::new(())),
            authorization_issuer: Arc::new(AuthorizationIssuer::new()),
        })
    }

    async fn readonly_existing_repository_pool(path: &Path) -> McpPlatformResult<Pool<Sqlite>> {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .read_only(true)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .map_err(map_sqlx)
    }

    pub fn database_path(&self) -> Option<&Path> {
        self.database_path.as_deref()
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    pub(crate) async fn verify_integrity(&self) -> McpPlatformResult<()> {
        self.begin_immediate().await?.rollback().await
    }

    pub(crate) async fn recovery_eligibility(&self) -> McpPlatformResult<RecoveryEligibility> {
        let mut transaction = self.begin_immediate().await?;
        let eligibility = recovery_eligibility_in_transaction(&mut transaction).await;
        transaction.rollback().await?;
        eligibility
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

    pub(crate) async fn save_manifest(
        &self,
        record: &ManifestRecord,
    ) -> McpPlatformResult<InsertOutcome> {
        validate_manifest_proof(&record.proof, record.verified.digest())?;
        record.source_metadata.validate()?;
        let mut tx = self.begin_immediate().await?;
        let outcome = save_manifest_in_tx(&mut tx, record).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(outcome)
    }

    pub(crate) async fn save_https_manifest_provision(
        &self,
        record: HttpsManifestProvisionRecord,
        token_hash: &str,
    ) -> McpPlatformResult<()> {
        let dns_evidence_digest = record.dns_evidence_digest.as_deref().ok_or_else(|| {
            error(
                McpPlatformErrorCode::ManifestConflict,
                "HTTPS manifest DNS evidence digest is required",
            )
        })?;
        if dns_evidence_digest.len() != 64
            || !dns_evidence_digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(error(
                McpPlatformErrorCode::ManifestConflict,
                "HTTPS manifest DNS evidence digest is invalid",
            ));
        }
        let requested_url = record.requested_url.as_deref().ok_or_else(|| {
            error(
                McpPlatformErrorCode::ManifestConflict,
                "HTTPS manifest requested URL is required",
            )
        })?;
        let parsed_url =
            crate::mcp_platform::https_manifest_policy::ValidatedHttpsUrl::parse(requested_url)
                .map_err(|_| {
                    error(
                        McpPlatformErrorCode::ManifestConflict,
                        "HTTPS manifest requested URL is invalid",
                    )
                })?;
        let requested_url = parsed_url.request_url().as_str();
        let requested_url_id = crate::utils::bytes_to_hex(Sha256::digest(requested_url.as_bytes()));
        if requested_url_id != record.requested_url_id {
            return Err(error(
                McpPlatformErrorCode::ManifestConflict,
                "HTTPS manifest requested URL identity mismatch",
            ));
        }
        if crate::utils::bytes_to_hex(Sha256::digest(&record.frozen_bytes)) != record.raw_digest {
            return Err(error(
                McpPlatformErrorCode::ManifestConflict,
                "HTTPS manifest raw digest mismatch",
            ));
        }
        if record.status != "saved" {
            return Err(error(
                McpPlatformErrorCode::ManifestConflict,
                "HTTPS manifest provision has invalid status",
            ));
        }
        let mut tx = self.begin_immediate().await?;
        if let Some(row) = sqlx::query("SELECT actor,transport_session_binding,requested_url,requested_url_id,final_url_id,raw_digest,parsed_digest,redirect_chain_digest,dns_evidence_digest,frozen_bytes,token_hash,expires_at_ms,status,created_at_ms FROM https_manifest_provisions WHERE provision_id=?")
            .bind(&record.provision_id).fetch_optional(&mut **tx).await.map_err(map_sqlx)? {
            let requested_url_id: Option<String> = row.try_get("requested_url_id").map_err(map_sqlx)?;
            let requested_url_id = requested_url_id.ok_or_else(integrity_error)?;
            let same = row.try_get::<String, _>("actor").map_err(map_sqlx)? == record.actor
                && row.try_get::<String, _>("transport_session_binding").map_err(map_sqlx)? == record.transport_session_binding
                && requested_url_id == record.requested_url_id
                && row.try_get::<Option<String>, _>("requested_url").map_err(map_sqlx)?.as_deref() == Some(requested_url)
                && row.try_get::<String, _>("final_url_id").map_err(map_sqlx)? == record.final_url_id
                && row.try_get::<Vec<u8>, _>("frozen_bytes").map_err(map_sqlx)? == record.frozen_bytes
                && row.try_get::<String, _>("token_hash").map_err(map_sqlx)? == token_hash
                && row.try_get::<String, _>("raw_digest").map_err(map_sqlx)? == record.raw_digest
                && row.try_get::<String, _>("parsed_digest").map_err(map_sqlx)? == record.parsed_digest
                && row.try_get::<String, _>("redirect_chain_digest").map_err(map_sqlx)? == record.redirect_chain_digest
                && row.try_get::<Option<String>, _>("dns_evidence_digest").map_err(map_sqlx)? == record.dns_evidence_digest
                && row.try_get::<i64, _>("expires_at_ms").map_err(map_sqlx)? == record.expires_at_ms
                && row.try_get::<String, _>("status").map_err(map_sqlx)? == record.status
                && row.try_get::<i64, _>("created_at_ms").map_err(map_sqlx)? == record.created_at_ms;
            if !same { return Err(error(McpPlatformErrorCode::IdempotencyConflict, "HTTPS manifest provision immutable conflict")); }
        } else {
            sqlx::query("INSERT INTO https_manifest_provisions (provision_id,actor,transport_session_binding,requested_url,requested_url_id,final_url_id,raw_digest,parsed_digest,redirect_chain_digest,dns_evidence_digest,frozen_bytes,token_hash,expires_at_ms,status,created_at_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
                .bind(&record.provision_id).bind(&record.actor).bind(&record.transport_session_binding).bind(requested_url).bind(&record.requested_url_id).bind(&record.final_url_id).bind(&record.raw_digest).bind(&record.parsed_digest).bind(&record.redirect_chain_digest).bind(dns_evidence_digest).bind(&record.frozen_bytes).bind(token_hash).bind(record.expires_at_ms).bind(&record.status).bind(record.created_at_ms)
                .execute(&mut **tx).await.map_err(map_sqlx)?;
        }
        tx.commit().await.map_err(map_sqlx)
    }

    pub(crate) async fn get_consumed_https_manifest_provision(
        &self,
        provision_id: &str,
        actor: &str,
        binding: &str,
    ) -> McpPlatformResult<HttpsManifestProvisionRecord> {
        let row = sqlx::query("SELECT provision_id,actor,transport_session_binding,requested_url,requested_url_id,final_url_id,raw_digest,parsed_digest,redirect_chain_digest,dns_evidence_digest,frozen_bytes,expires_at_ms,status,created_at_ms FROM https_manifest_provisions WHERE provision_id=? AND actor=? AND transport_session_binding=? AND status='consumed'")
            .bind(provision_id)
            .bind(actor)
            .bind(binding)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(|| error(McpPlatformErrorCode::NotFound, "HTTPS manifest provision is unavailable"))?;
        let requested_url: Option<String> = row.try_get("requested_url").map_err(map_sqlx)?;
        let requested_url_id: Option<String> = row.try_get("requested_url_id").map_err(map_sqlx)?;
        let requested_url_id = requested_url_id.ok_or_else(integrity_error)?;
        validate_stored_requested_url(&requested_url, &requested_url_id)?;
        let dns_evidence_digest = row
            .try_get::<Option<String>, _>("dns_evidence_digest")
            .map_err(map_sqlx)?
            .ok_or_else(integrity_error)?;
        Ok(HttpsManifestProvisionRecord {
            provision_id: row.try_get("provision_id").map_err(map_sqlx)?,
            actor: row.try_get("actor").map_err(map_sqlx)?,
            transport_session_binding: row
                .try_get("transport_session_binding")
                .map_err(map_sqlx)?,
            requested_url,
            requested_url_id,
            final_url_id: row.try_get("final_url_id").map_err(map_sqlx)?,
            raw_digest: row.try_get("raw_digest").map_err(map_sqlx)?,
            parsed_digest: row.try_get("parsed_digest").map_err(map_sqlx)?,
            redirect_chain_digest: row.try_get("redirect_chain_digest").map_err(map_sqlx)?,
            dns_evidence_digest: Some(dns_evidence_digest),
            frozen_bytes: row.try_get("frozen_bytes").map_err(map_sqlx)?,
            expires_at_ms: row.try_get("expires_at_ms").map_err(map_sqlx)?,
            status: row.try_get("status").map_err(map_sqlx)?,
            created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
        })
    }

    pub(crate) async fn get_manifest_for_https_source_binding(
        &self,
        binding: &crate::mcp_platform::repository::HttpsManifestSourceBinding,
    ) -> McpPlatformResult<ManifestRecord> {
        let record = sqlx::query("SELECT provision_id,actor,transport_session_binding,requested_url,requested_url_id,final_url_id,raw_digest,parsed_digest,redirect_chain_digest,dns_evidence_digest,frozen_bytes,expires_at_ms,status,created_at_ms FROM https_manifest_provisions WHERE provision_id=? AND status='consumed'")
            .bind(&binding.provision_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(integrity_error)?;
        let requested_url: Option<String> = record.try_get("requested_url").map_err(map_sqlx)?;
        let requested_url = requested_url.ok_or_else(integrity_error)?;
        let frozen_bytes: Vec<u8> = record.try_get("frozen_bytes").map_err(map_sqlx)?;
        let dns_evidence_digest: Option<String> =
            record.try_get("dns_evidence_digest").map_err(map_sqlx)?;
        let dns_evidence_digest = dns_evidence_digest.ok_or_else(integrity_error)?;
        let matches = record
            .try_get::<String, _>("requested_url_id")
            .map_err(map_sqlx)?
            == binding.requested_url_id
            && record
                .try_get::<String, _>("final_url_id")
                .map_err(map_sqlx)?
                == binding.final_url_id
            && record
                .try_get::<String, _>("raw_digest")
                .map_err(map_sqlx)?
                == binding.raw_digest
            && record
                .try_get::<String, _>("parsed_digest")
                .map_err(map_sqlx)?
                == binding.parsed_digest
            && record
                .try_get::<String, _>("redirect_chain_digest")
                .map_err(map_sqlx)?
                == binding.redirect_chain_digest
            && dns_evidence_digest == binding.dns_evidence_digest
            && crate::utils::bytes_to_hex(Sha256::digest(requested_url.as_bytes()))
                == binding.requested_url_id
            && crate::utils::bytes_to_hex(Sha256::digest(&frozen_bytes)) == binding.raw_digest;
        if !matches {
            return Err(integrity_error());
        }
        let verified = parse_manifest(&frozen_bytes).map_err(|_| integrity_error())?;
        if verified.digest() != binding.parsed_digest {
            return Err(integrity_error());
        }
        Ok(ManifestRecord {
            verified,
            proof: ManifestProof::LocalBytes,
            trust_tier: crate::mcp_platform::TrustTier::Local,
            source_metadata: ManifestSourceMetadata {
                source_ref: SourceRef::HttpsManifestUrl {
                    manifest_url: requested_url,
                },
                import_kind: SourceImportKind::HttpsManifestUrl,
                ..ManifestSourceMetadata::default()
            },
            created_at_ms: record.try_get("created_at_ms").map_err(map_sqlx)?,
        })
    }

    pub(crate) async fn claim_https_manifest_provision(
        &self,
        provision_id: &str,
        token_hash: &str,
        actor: &str,
        binding: &str,
        now_ms: i64,
    ) -> McpPlatformResult<HttpsManifestProvisionRecord> {
        let mut tx = self.begin_immediate().await?;
        let candidate = sqlx::query("SELECT requested_url,requested_url_id,dns_evidence_digest FROM https_manifest_provisions WHERE provision_id=? AND token_hash=? AND actor=? AND transport_session_binding=? AND status='saved' AND expires_at_ms>?")
            .bind(provision_id).bind(token_hash).bind(actor).bind(binding).bind(now_ms)
            .fetch_optional(&mut **tx).await.map_err(map_sqlx)?;
        let candidate = candidate.ok_or_else(|| {
            error(
                McpPlatformErrorCode::NotFound,
                "HTTPS manifest provision is unavailable",
            )
        })?;
        let requested_url_id: Option<String> =
            candidate.try_get("requested_url_id").map_err(map_sqlx)?;
        let requested_url_id = requested_url_id.ok_or_else(integrity_error)?;
        validate_stored_requested_url(
            &candidate.try_get("requested_url").map_err(map_sqlx)?,
            &requested_url_id,
        )?;
        candidate
            .try_get::<Option<String>, _>("dns_evidence_digest")
            .map_err(map_sqlx)?
            .ok_or_else(integrity_error)?;
        let changed = sqlx::query("UPDATE https_manifest_provisions SET status='claimed',claimed_at_ms=?,claim_lease_until_ms=? WHERE provision_id=? AND token_hash=? AND actor=? AND transport_session_binding=? AND status='saved' AND expires_at_ms>?")
            .bind(now_ms).bind(now_ms.saturating_add(migrations::HTTPS_MANIFEST_CLAIM_LEASE_MS)).bind(provision_id).bind(token_hash).bind(actor).bind(binding).bind(now_ms).execute(&mut **tx).await.map_err(map_sqlx)?;
        if changed.rows_affected() != 1 {
            return Err(error(
                McpPlatformErrorCode::NotFound,
                "HTTPS manifest provision is unavailable",
            ));
        }
        let row = sqlx::query("SELECT provision_id,actor,transport_session_binding,requested_url,requested_url_id,final_url_id,raw_digest,parsed_digest,redirect_chain_digest,dns_evidence_digest,frozen_bytes,token_hash,expires_at_ms,status,created_at_ms FROM https_manifest_provisions WHERE provision_id=?")
            .bind(provision_id).fetch_one(&mut **tx).await.map_err(map_sqlx)?;
        let requested_url_id: Option<String> = row.try_get("requested_url_id").map_err(map_sqlx)?;
        let requested_url_id = requested_url_id.ok_or_else(integrity_error)?;
        let record = HttpsManifestProvisionRecord {
            provision_id: row.try_get("provision_id").map_err(map_sqlx)?,
            actor: row.try_get("actor").map_err(map_sqlx)?,
            transport_session_binding: row
                .try_get("transport_session_binding")
                .map_err(map_sqlx)?,
            requested_url_id,
            requested_url: row.try_get("requested_url").map_err(map_sqlx)?,
            final_url_id: row.try_get("final_url_id").map_err(map_sqlx)?,
            raw_digest: row.try_get("raw_digest").map_err(map_sqlx)?,
            parsed_digest: row.try_get("parsed_digest").map_err(map_sqlx)?,
            redirect_chain_digest: row.try_get("redirect_chain_digest").map_err(map_sqlx)?,
            dns_evidence_digest: Some(
                row.try_get::<Option<String>, _>("dns_evidence_digest")
                    .map_err(map_sqlx)?
                    .ok_or_else(integrity_error)?,
            ),
            frozen_bytes: row.try_get("frozen_bytes").map_err(map_sqlx)?,
            expires_at_ms: row.try_get("expires_at_ms").map_err(map_sqlx)?,
            status: row.try_get("status").map_err(map_sqlx)?,
            created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
        };
        tx.commit().await.map_err(map_sqlx)?;
        Ok(record)
    }

    pub(crate) async fn consume_https_manifest_provision(
        &self,
        provision_id: &str,
        token_hash: &str,
        actor: &str,
        binding: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.finish_https_manifest_provision(
            provision_id,
            token_hash,
            actor,
            binding,
            now_ms,
            "consumed",
            false,
        )
        .await
    }

    pub(crate) async fn finalize_https_manifest_provision(
        &self,
        provision_id: &str,
        token_hash: &str,
        actor: &str,
        binding: &str,
        now_ms: i64,
        manifest: &ManifestRecord,
    ) -> McpPlatformResult<InsertOutcome> {
        let requested_url = match &manifest.source_metadata.source_ref {
            crate::mcp_platform::manifest::SourceRef::HttpsManifestUrl { manifest_url } => {
                manifest_url.as_str()
            }
            _ => {
                return Err(error(
                    McpPlatformErrorCode::ManifestConflict,
                    "HTTPS manifest source URL is required",
                ));
            }
        };
        let requested_url = canonical_requested_url(requested_url)?;
        let mut tx = self.begin_immediate().await?;
        let row = sqlx::query("SELECT requested_url,requested_url_id,dns_evidence_digest FROM https_manifest_provisions WHERE provision_id=? AND token_hash=? AND actor=? AND transport_session_binding=? AND status='claimed' AND expires_at_ms>? AND claim_lease_until_ms>?")
            .bind(provision_id)
            .bind(token_hash)
            .bind(actor)
            .bind(binding)
            .bind(now_ms)
            .bind(now_ms)
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(|| error(McpPlatformErrorCode::NotFound, "HTTPS manifest provision is unavailable"))?;
        let stored_url: Option<String> = row.try_get("requested_url").map_err(map_sqlx)?;
        let stored_url = stored_url.ok_or_else(integrity_error)?;
        let url_id: Option<String> = row.try_get("requested_url_id").map_err(map_sqlx)?;
        let url_id = url_id.ok_or_else(integrity_error)?;
        validate_stored_requested_url(&Some(stored_url.clone()), &url_id)?;
        row.try_get::<Option<String>, _>("dns_evidence_digest")
            .map_err(map_sqlx)?
            .ok_or_else(integrity_error)?;
        if stored_url != requested_url
            || url_id != crate::utils::bytes_to_hex(Sha256::digest(stored_url.as_bytes()))
        {
            return Err(error(
                McpPlatformErrorCode::NotFound,
                "HTTPS manifest provision is unavailable",
            ));
        }
        let outcome = save_manifest_in_tx(&mut tx, manifest).await?;
        sqlx::query("UPDATE https_manifest_provisions SET status='consumed',terminal_at_ms=? WHERE provision_id=? AND status='claimed'")
            .bind(now_ms).bind(provision_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(outcome)
    }

    pub(crate) async fn reject_https_manifest_provision(
        &self,
        provision_id: &str,
        token_hash: &str,
        actor: &str,
        binding: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.finish_https_manifest_provision(
            provision_id,
            token_hash,
            actor,
            binding,
            now_ms,
            "rejected",
            true,
        )
        .await
    }

    async fn finish_https_manifest_provision(
        &self,
        provision_id: &str,
        token_hash: &str,
        actor: &str,
        binding: &str,
        now_ms: i64,
        status: &str,
        allow_expired: bool,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        if !allow_expired {
            let row = sqlx::query("SELECT requested_url,requested_url_id,dns_evidence_digest FROM https_manifest_provisions WHERE provision_id=? AND token_hash=? AND actor=? AND transport_session_binding=? AND status='claimed'")
                .bind(provision_id).bind(token_hash).bind(actor).bind(binding).fetch_optional(&mut **tx).await.map_err(map_sqlx)?
                .ok_or_else(|| error(McpPlatformErrorCode::NotFound, "HTTPS manifest provision is unavailable"))?;
            let requested_url_id: Option<String> =
                row.try_get("requested_url_id").map_err(map_sqlx)?;
            let requested_url_id = requested_url_id.ok_or_else(integrity_error)?;
            validate_stored_requested_url(
                &row.try_get("requested_url").map_err(map_sqlx)?,
                &requested_url_id,
            )?;
            row.try_get::<Option<String>, _>("dns_evidence_digest")
                .map_err(map_sqlx)?
                .ok_or_else(integrity_error)?;
        }
        let sql = if allow_expired {
            "UPDATE https_manifest_provisions SET status=?,terminal_at_ms=? WHERE provision_id=? AND token_hash=? AND actor=? AND transport_session_binding=? AND status='claimed' AND claim_lease_until_ms IS NOT NULL"
        } else {
            "UPDATE https_manifest_provisions SET status=?,terminal_at_ms=? WHERE provision_id=? AND token_hash=? AND actor=? AND transport_session_binding=? AND status='claimed' AND expires_at_ms>? AND claim_lease_until_ms>?"
        };
        let mut query = sqlx::query(sql)
            .bind(status)
            .bind(now_ms)
            .bind(provision_id)
            .bind(token_hash)
            .bind(actor)
            .bind(binding);
        if !allow_expired {
            query = query.bind(now_ms).bind(now_ms);
        }
        if query
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?
            .rows_affected()
            != 1
        {
            return Err(error(
                McpPlatformErrorCode::NotFound,
                "HTTPS manifest provision is unavailable",
            ));
        }
        tx.commit().await.map_err(map_sqlx)
    }

    pub(crate) async fn save_governed_catalog_import(
        &self,
        input: SaveGovernedCatalogImport,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        if governed_source_refresh_trust_anchor_exists_for_import_in_tx(&mut tx, &input).await? {
            return Err(anchored_source_reimport_denied());
        }
        let replace_existing_source_entries = input.verified_source_document.is_some();
        if let Some(document) = &input.verified_source_document {
            crate::verified_source_catalog::dao::store_verified_document_tx(
                &mut tx,
                document,
                input.document.created_at_ms,
            )
            .await
            .map_err(|_| repository_unavailable())?;
        }
        save_governed_catalog_import_records_in_tx(
            &mut tx,
            &input,
            replace_existing_source_entries,
        )
        .await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }
}

fn canonical_requested_url(value: &str) -> McpPlatformResult<String> {
    crate::mcp_platform::https_manifest_policy::ValidatedHttpsUrl::parse(value)
        .map(|url| url.request_url().as_str().to_owned())
        .map_err(|_| {
            error(
                McpPlatformErrorCode::ManifestConflict,
                "HTTPS manifest requested URL is invalid",
            )
        })
}

fn validate_stored_requested_url(url: &Option<String>, id: &str) -> McpPlatformResult<()> {
    let url = url.as_deref().ok_or_else(|| {
        error(
            McpPlatformErrorCode::IntegrityError,
            "HTTPS manifest provision requested URL is unavailable",
        )
    })?;
    let canonical = canonical_requested_url(url)?;
    let expected = crate::utils::bytes_to_hex(Sha256::digest(canonical.as_bytes()));
    if canonical != url
        || id != expected
        || id.len() != 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(error(
            McpPlatformErrorCode::IntegrityError,
            "HTTPS manifest provision requested URL binding is invalid",
        ));
    }
    Ok(())
}

async fn governed_source_refresh_trust_anchor_exists_for_import_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &SaveGovernedCatalogImport,
) -> McpPlatformResult<bool> {
    let source_ref_verified_source_id = match &input.source.source_ref {
        SourceRef::VerifiedSourceCatalog { source_id } => Some(source_id.as_str()),
        _ => None,
    };
    let document_verified_source_id = input
        .verified_source_document
        .as_ref()
        .map(|document| document.envelope.payload.source_id.as_str());
    sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
                SELECT 1
                FROM governed_source_refresh_trust_anchors
                WHERE source_id = ?
                   OR source_id = ?
                   OR (? IS NOT NULL AND verified_source_id = ?)
                   OR (? IS NOT NULL AND verified_source_id = ?)
           )"#,
    )
    .bind(&input.source.source_id)
    .bind(&input.document.source_id)
    .bind(source_ref_verified_source_id)
    .bind(source_ref_verified_source_id)
    .bind(document_verified_source_id)
    .bind(document_verified_source_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)
}

async fn validate_verified_catalog_entries_not_revoked_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &SaveGovernedCatalogImport,
) -> McpPlatformResult<()> {
    let SourceRef::VerifiedSourceCatalog { source_id } = &input.source.source_ref else {
        return Ok(());
    };

    for entry in &input.entries {
        let SourceRef::VerifiedSourceCatalog {
            source_id: entry_source_id,
        } = &entry.manifest.source_metadata.source_ref
        else {
            return Err(integrity_error());
        };
        let Some(release_id) = entry.manifest.source_metadata.release_id.as_deref() else {
            return Err(integrity_error());
        };
        if entry_source_id != source_id {
            return Err(integrity_error());
        }
        let revoked = sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                    SELECT 1
                    FROM source_revocations
                    WHERE source_id = ? AND release_id = ?
               )"#,
        )
        .bind(source_id)
        .bind(release_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if revoked {
            return Err(error(
                McpPlatformErrorCode::IntegrityError,
                "revoked verified source release cannot be exposed through governed catalog",
            ));
        }
    }

    Ok(())
}

fn anchored_source_reimport_denied() -> crate::mcp_platform::McpPlatformError {
    error(
        McpPlatformErrorCode::OperationNotSupported,
        "governed source is refresh-anchored; use governed source refresh",
    )
}

fn validate_source_provisioning_commit_input(
    input: &ConfirmSourceProvisioning,
    parsed: &crate::mcp_platform::source_provisioning::ParsedSourceProvisioningSnapshot,
) -> McpPlatformResult<()> {
    if input.provision_id.is_empty()
        || input.provision_id.len() > 256
        || input.actor.is_empty()
        || input.actor.len() > 256
        || input.correlation_id != input.provision_id
        || input.snapshot_digest != parsed.snapshot_digest
        || input.snapshot_digest
            != crate::mcp_platform::source_provisioning::source_provisioning_snapshot_digest(
                &input.frozen,
            )
    {
        return Err(integrity_error());
    }

    let expected_source_id = format!("verified_source_catalog_{}", parsed.descriptor.source_id);
    let expected_document_id = format!(
        "directory_{}",
        parsed.source_document.digests.document_digest
    );
    let Some(document) = input.import.verified_source_document.as_ref() else {
        return Err(integrity_error());
    };
    if document.raw_bytes != input.frozen.source_document_bytes
        || document != &parsed.source_document
        || input.import.source.source_id != expected_source_id
        || input.import.source.import_kind != SourceImportKind::VerifiedSourceCatalog
        || input.import.source.source_ref
            != (SourceRef::VerifiedSourceCatalog {
                source_id: parsed.descriptor.source_id.clone(),
            })
        || input.import.source.display_name != parsed.source_document.envelope.payload.source_name
        || input.import.source.created_at_ms != input.occurred_at_ms
        || input.import.source.updated_at_ms != input.occurred_at_ms
        || input.import.document.source_id != expected_source_id
        || input.import.document.document_id != expected_document_id
        || input.import.document.document_digest != parsed.source_document.digests.document_digest
        || input.import.document.document_kind != GovernedCatalogDocumentKind::Directory
        || input.import.document.created_at_ms != input.occurred_at_ms
        || input.import.entries.len() != parsed.manifest_count as usize
    {
        return Err(integrity_error());
    }

    let mut frozen_manifests = std::collections::BTreeMap::new();
    for frozen_manifest in &input.frozen.manifests {
        let verified =
            parse_manifest(&frozen_manifest.document_bytes).map_err(|_| integrity_error())?;
        if frozen_manifests
            .insert(frozen_manifest.release_id.as_str(), verified)
            .is_some()
        {
            return Err(integrity_error());
        }
    }
    if frozen_manifests.len() != parsed.manifest_count as usize {
        return Err(integrity_error());
    }

    let provenance = VerifiedSourceDocumentRef {
        source_id: parsed.descriptor.source_id.clone(),
        document_digest: parsed.source_document.digests.document_digest.clone(),
        canonical_digest: parsed.source_document.digests.canonical_digest.clone(),
        signed_digest: parsed.source_document.digests.signed_digest.clone(),
        binding_digest: parsed.source_document.digests.binding_digest.clone(),
        signature_kids: parsed
            .source_document
            .envelope
            .signatures
            .iter()
            .map(|signature| signature.kid.clone())
            .collect(),
    };
    let mut seen_release_ids = std::collections::BTreeSet::new();
    for entry in &input.import.entries {
        let Some(frozen_verified) = frozen_manifests.get(entry.entry_id.as_str()) else {
            return Err(integrity_error());
        };
        let expected_metadata = ManifestSourceMetadata {
            source_ref: SourceRef::VerifiedSourceCatalog {
                source_id: parsed.descriptor.source_id.clone(),
            },
            import_kind: SourceImportKind::VerifiedSourceCatalog,
            release_id: Some(entry.entry_id.clone()),
            origin_provenance: OriginProvenance {
                verified_source_document: Some(provenance.clone()),
            },
            update_channel: UpdateChannel::Default,
        };
        let expected_proof = ManifestProof::Catalog {
            index_digest: parsed.source_document.digests.document_digest.clone(),
            declared_manifest_digest: frozen_verified.digest().to_string(),
            signature: None,
        };
        if !seen_release_ids.insert(entry.entry_id.as_str())
            || entry.manifest.verified != *frozen_verified
            || entry.manifest.source_metadata != expected_metadata
            || entry.manifest.proof != expected_proof
            || entry.manifest.trust_tier != crate::mcp_platform::TrustTier::Official
            || entry.manifest.created_at_ms != input.occurred_at_ms
        {
            return Err(integrity_error());
        }
    }
    Ok(())
}

async fn reject_existing_source_provisioning_state_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &ConfirmSourceProvisioning,
    parsed: &crate::mcp_platform::source_provisioning::ParsedSourceProvisioningSnapshot,
) -> McpPlatformResult<()> {
    let governed_source_id = format!("verified_source_catalog_{}", parsed.descriptor.source_id);
    let exists = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
                SELECT 1 FROM governed_catalog_sources WHERE source_id = ?
                UNION ALL
                SELECT 1 FROM governed_catalog_documents WHERE source_id = ?
                UNION ALL
                SELECT 1 FROM governed_catalog_entries WHERE source_id = ?
                UNION ALL
                SELECT 1 FROM source_catalog_documents WHERE source_id = ?
                UNION ALL
                SELECT 1 FROM source_revocations WHERE source_id = ?
                UNION ALL
                SELECT 1 FROM governed_source_refresh_registrations WHERE source_id = ?
                UNION ALL
                SELECT 1 FROM governed_source_refresh_trust_anchors WHERE source_id = ?
                UNION ALL
                SELECT 1 FROM governed_source_trust_pins WHERE source_id = ? OR verified_source_id = ?
                UNION ALL
                SELECT 1 FROM governed_source_provision_audits WHERE provision_id = ? OR source_id = ?
           )"#,
    )
    .bind(&governed_source_id)
    .bind(&governed_source_id)
    .bind(&governed_source_id)
    .bind(&parsed.descriptor.source_id)
    .bind(&parsed.descriptor.source_id)
    .bind(&governed_source_id)
    .bind(&governed_source_id)
    .bind(&governed_source_id)
    .bind(&parsed.descriptor.source_id)
    .bind(&input.provision_id)
    .bind(&governed_source_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    if exists {
        return Err(error(
            McpPlatformErrorCode::OperationNotSupported,
            "source provisioning requires a previously unseen verified source",
        ));
    }
    Ok(())
}

async fn save_governed_catalog_import_records_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &SaveGovernedCatalogImport,
    replace_existing_source_entries: bool,
) -> McpPlatformResult<()> {
    validate_verified_catalog_entries_not_revoked_in_tx(tx, input).await?;

    sqlx::query(
        r#"INSERT INTO governed_catalog_sources(
                source_id, import_kind, source_ref_json, display_name, created_at_ms, updated_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(source_id) DO UPDATE SET
                import_kind = excluded.import_kind,
                source_ref_json = excluded.source_ref_json,
                display_name = excluded.display_name,
                updated_at_ms = excluded.updated_at_ms"#,
    )
    .bind(&input.source.source_id)
    .bind(input.source.import_kind.as_str())
    .bind(encode(&input.source.source_ref)?)
    .bind(&input.source.display_name)
    .bind(input.source.created_at_ms)
    .bind(input.source.updated_at_ms)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx)?;

    if replace_existing_source_entries {
        sqlx::query("DELETE FROM governed_catalog_entries WHERE source_id = ?")
            .bind(&input.source.source_id)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        sqlx::query("DELETE FROM governed_catalog_documents WHERE source_id = ?")
            .bind(&input.source.source_id)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }

    sqlx::query(
        r#"INSERT INTO governed_catalog_documents(
                source_id, document_id, document_digest, document_kind, created_at_ms
            ) VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(source_id, document_id) DO UPDATE SET
                document_digest = excluded.document_digest,
                document_kind = excluded.document_kind,
                created_at_ms = excluded.created_at_ms"#,
    )
    .bind(&input.document.source_id)
    .bind(&input.document.document_id)
    .bind(&input.document.document_digest)
    .bind(governed_document_kind_sql(&input.document.document_kind))
    .bind(input.document.created_at_ms)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx)?;

    for entry in &input.entries {
        save_manifest_in_tx(tx, &entry.manifest).await?;
        if let Some(existing) = sqlx::query(
            r#"SELECT manifest_digest, proof_json, trust_tier_json, source_metadata_json
                   FROM governed_catalog_entries
                   WHERE source_id = ? AND mcp_id = ? AND version = ?"#,
        )
        .bind(&input.source.source_id)
        .bind(&entry.manifest.verified.manifest().id)
        .bind(entry.manifest.verified.manifest().version.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        {
            let existing_digest: String = existing.try_get("manifest_digest").map_err(map_sqlx)?;
            let existing_proof: ManifestProof = decode(
                existing
                    .try_get::<&str, _>("proof_json")
                    .map_err(map_sqlx)?,
            )?;
            let existing_trust: crate::mcp_platform::TrustTier = decode(
                existing
                    .try_get::<&str, _>("trust_tier_json")
                    .map_err(map_sqlx)?,
            )?;
            let existing_metadata: ManifestSourceMetadata = decode(
                existing
                    .try_get::<&str, _>("source_metadata_json")
                    .map_err(map_sqlx)?,
            )?;
            if existing_digest != entry.manifest.verified.digest()
                || existing_proof != entry.manifest.proof
                || existing_trust != entry.manifest.trust_tier
                || existing_metadata != entry.manifest.source_metadata
            {
                return Err(error(
                    McpPlatformErrorCode::ManifestConflict,
                    "governed catalog identity already has different immutable content",
                ));
            }
            continue;
        }
        sqlx::query(
            r#"INSERT INTO governed_catalog_entries(
                    source_id, entry_id, document_id, manifest_digest, mcp_id, version,
                    proof_json, trust_tier_json, source_metadata_json, created_at_ms
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&input.source.source_id)
        .bind(&entry.entry_id)
        .bind(&input.document.document_id)
        .bind(entry.manifest.verified.digest())
        .bind(&entry.manifest.verified.manifest().id)
        .bind(entry.manifest.verified.manifest().version.as_str())
        .bind(encode(&entry.manifest.proof)?)
        .bind(encode(&entry.manifest.trust_tier)?)
        .bind(encode(&entry.manifest.source_metadata)?)
        .bind(entry.manifest.created_at_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    }

    Ok(())
}

impl SqliteMcpPlatformRepository {
    pub async fn get_manifest(&self, digest: &str) -> McpPlatformResult<ManifestRecord> {
        match self.list_governed_catalog_entries_by_digest(digest).await {
            Ok(mut entries) => {
                if entries.len() != 1 {
                    return Err(error(
                        McpPlatformErrorCode::InvalidRequest,
                        "manifest digest requires an explicit source selection",
                    ));
                }
                return Ok(entries.remove(0).manifest);
            }
            Err(error) if error.code() != McpPlatformErrorCode::NotFound => return Err(error),
            Err(_) => {}
        }
        let row = sqlx::query(
            r#"SELECT manifest_digest, mcp_id, version, canonical_bytes, proof_json,
                trust_tier_json, source_ref_json, import_kind, release_id,
                origin_provenance_json, update_channel_json, created_at_ms
                FROM manifest_blobs WHERE manifest_digest = ?"#,
        )
        .bind(digest)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        decode_manifest_row(&row)
    }

    pub async fn get_manifest_with_source_context(
        &self,
        digest: &str,
        source_context: &ManifestSourceContext,
    ) -> McpPlatformResult<ManifestRecord> {
        match source_context {
            ManifestSourceContext::HttpsProvision { .. } => Err(error(
                McpPlatformErrorCode::InvalidRequest,
                "HTTPS provision manifests require the trusted in-memory source binding",
            )),
            ManifestSourceContext::GovernedCatalog { source_id } => {
                let mut transaction = self.begin_verified_read_snapshot().await?;
                let result = self
                    .get_governed_catalog_entry_by_source_and_digest_in_tx(
                        &mut transaction,
                        source_id,
                        digest,
                    )
                    .await
                    .map(|entry| entry.manifest);
                finish_verified_read_snapshot(transaction, result).await
            }
            ManifestSourceContext::PersistedManifest { source_id } => {
                let manifest = self.get_manifest(digest).await?;
                if persisted_source_id(&manifest.source_metadata)? != *source_id {
                    return Err(error(
                        McpPlatformErrorCode::InvalidRequest,
                        "manifest source context no longer matches persisted provenance",
                    ));
                }
                Ok(manifest)
            }
        }
    }

    pub async fn list_manifests(&self) -> McpPlatformResult<Vec<ManifestRecord>> {
        let rows = sqlx::query(
            r#"SELECT manifest_digest, mcp_id, version, canonical_bytes, proof_json,
                trust_tier_json, source_ref_json, import_kind, release_id,
                origin_provenance_json, update_channel_json, created_at_ms FROM manifest_blobs
                ORDER BY mcp_id, version, manifest_digest"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        rows.iter().map(decode_manifest_row).collect()
    }

    pub async fn list_manifests_with_source_context(
        &self,
        source_context: &ManifestSourceContext,
    ) -> McpPlatformResult<Vec<ManifestRecord>> {
        match source_context {
            ManifestSourceContext::HttpsProvision { .. } => Err(error(
                McpPlatformErrorCode::InvalidRequest,
                "HTTPS provision source context cannot be resolved from a digest",
            )),
            ManifestSourceContext::GovernedCatalog { source_id } => Ok(self
                .list_governed_catalog_entries()
                .await?
                .into_iter()
                .filter(|entry| entry.source.source_id == *source_id)
                .map(|entry| entry.manifest)
                .collect()),
            ManifestSourceContext::PersistedManifest { source_id } => Ok(self
                .list_manifests()
                .await?
                .into_iter()
                .filter(|manifest| {
                    persisted_source_id(&manifest.source_metadata)
                        .is_ok_and(|persisted| persisted == *source_id)
                })
                .collect()),
        }
    }

    pub async fn get_manifest_by_identity(
        &self,
        mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<ManifestRecord> {
        let row = sqlx::query(
            r#"SELECT manifest_digest, mcp_id, version, canonical_bytes, proof_json,
                trust_tier_json, source_ref_json, import_kind, release_id,
                origin_provenance_json, update_channel_json, created_at_ms FROM manifest_blobs
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

    pub async fn get_manifest_by_identity_with_source_context(
        &self,
        mcp_id: &str,
        version: &str,
        source_context: &ManifestSourceContext,
    ) -> McpPlatformResult<ManifestRecord> {
        match source_context {
            ManifestSourceContext::HttpsProvision { .. } => Err(error(
                McpPlatformErrorCode::InvalidRequest,
                "HTTPS provision source context requires the stored plan binding",
            )),
            ManifestSourceContext::GovernedCatalog { source_id } => Ok(self
                .get_governed_catalog_entry(source_id, mcp_id, version)
                .await?
                .manifest),
            ManifestSourceContext::PersistedManifest { source_id } => {
                let manifest = self.get_manifest_by_identity(mcp_id, version).await?;
                if persisted_source_id(&manifest.source_metadata)? != *source_id {
                    return Err(error(
                        McpPlatformErrorCode::InvalidRequest,
                        "manifest source context no longer matches persisted provenance",
                    ));
                }
                Ok(manifest)
            }
        }
    }

    pub async fn list_governed_catalog_entries(
        &self,
    ) -> McpPlatformResult<Vec<GovernedCatalogEntryRecord>> {
        let mut transaction = self.begin_verified_read_snapshot().await?;
        let result = self
            .list_governed_catalog_entries_in_tx(&mut transaction)
            .await;
        finish_verified_read_snapshot(transaction, result).await
    }

    async fn list_governed_catalog_entries_in_tx(
        &self,
        transaction: &mut Transaction<'_, Sqlite>,
    ) -> McpPlatformResult<Vec<GovernedCatalogEntryRecord>> {
        let query = format!(
            "{GOVERNED_CATALOG_ENTRY_PROJECTION}\nORDER BY e.mcp_id, e.version, e.source_id, e.manifest_digest"
        );
        let rows = sqlx::query(&query)
            .fetch_all(&mut **transaction)
            .await
            .map_err(map_sqlx)?;
        let entry_count =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM governed_catalog_entries")
                .fetch_one(&mut **transaction)
                .await
                .map_err(map_sqlx)?;
        if rows.len() != entry_count as usize {
            return Err(integrity_error());
        }
        rows.iter().map(decode_governed_catalog_entry_row).collect()
    }

    pub async fn list_governed_catalog_sources(
        &self,
    ) -> McpPlatformResult<Vec<GovernedCatalogSourceRecord>> {
        let mut transaction = self.begin_verified_read_snapshot().await?;
        let result = self
            .list_governed_catalog_sources_in_tx(&mut transaction)
            .await;
        finish_verified_read_snapshot(transaction, result).await
    }

    async fn list_governed_catalog_sources_in_tx(
        &self,
        transaction: &mut Transaction<'_, Sqlite>,
    ) -> McpPlatformResult<Vec<GovernedCatalogSourceRecord>> {
        let rows = sqlx::query(
            r#"SELECT
                    source_id, import_kind, source_ref_json, display_name,
                    created_at_ms, updated_at_ms
               FROM governed_catalog_sources
               ORDER BY source_id"#,
        )
        .fetch_all(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        rows.iter()
            .map(decode_governed_catalog_source_row)
            .collect()
    }

    pub async fn get_governed_catalog_source(
        &self,
        source_id: &str,
    ) -> McpPlatformResult<GovernedCatalogSourceRecord> {
        let mut transaction = self.begin_verified_read_snapshot().await?;
        let result = self
            .get_governed_catalog_source_in_tx(&mut transaction, source_id)
            .await;
        finish_verified_read_snapshot(transaction, result).await
    }

    async fn get_governed_catalog_source_in_tx(
        &self,
        transaction: &mut Transaction<'_, Sqlite>,
        source_id: &str,
    ) -> McpPlatformResult<GovernedCatalogSourceRecord> {
        let row = sqlx::query(
            r#"SELECT
                    source_id, import_kind, source_ref_json, display_name,
                    created_at_ms, updated_at_ms
               FROM governed_catalog_sources
               WHERE source_id = ?"#,
        )
        .bind(source_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        decode_governed_catalog_source_row(&row)
    }

    pub async fn get_governed_catalog_entry(
        &self,
        source_id: &str,
        mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<GovernedCatalogEntryRecord> {
        let mut transaction = self.begin_verified_read_snapshot().await?;
        let result = self
            .get_governed_catalog_entry_in_tx(&mut transaction, source_id, mcp_id, version)
            .await;
        finish_verified_read_snapshot(transaction, result).await
    }

    async fn get_governed_catalog_entry_in_tx(
        &self,
        transaction: &mut Transaction<'_, Sqlite>,
        source_id: &str,
        mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<GovernedCatalogEntryRecord> {
        let query = format!(
            "{GOVERNED_CATALOG_ENTRY_PROJECTION}\nWHERE e.source_id = ? AND e.mcp_id = ? AND e.version = ?"
        );
        let row = sqlx::query(&query)
            .bind(source_id)
            .bind(mcp_id)
            .bind(version)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(map_sqlx)?;
        let Some(row) = row else {
            let entry_exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM governed_catalog_entries WHERE source_id = ? AND mcp_id = ? AND version = ?)",
            )
            .bind(source_id)
            .bind(mcp_id)
            .bind(version)
            .fetch_one(&mut **transaction)
            .await
            .map_err(map_sqlx)?;
            return if entry_exists {
                Err(integrity_error())
            } else {
                Err(not_found())
            };
        };
        decode_governed_catalog_entry_row(&row)
    }

    pub async fn get_governed_catalog_entry_by_digest(
        &self,
        manifest_digest: &str,
    ) -> McpPlatformResult<GovernedCatalogEntryRecord> {
        let mut entries = self
            .list_governed_catalog_entries_by_digest(manifest_digest)
            .await?;
        if entries.len() > 1 {
            return Err(error(
                McpPlatformErrorCode::InvalidRequest,
                "manifest digest requires an explicit source selection",
            ));
        }
        entries.pop().ok_or_else(not_found)
    }

    pub async fn list_governed_catalog_entries_by_digest(
        &self,
        manifest_digest: &str,
    ) -> McpPlatformResult<Vec<GovernedCatalogEntryRecord>> {
        let mut transaction = self.begin_verified_read_snapshot().await?;
        let result = self
            .list_governed_catalog_entries_by_digest_in_tx(&mut transaction, manifest_digest)
            .await;
        finish_verified_read_snapshot(transaction, result).await
    }

    async fn list_governed_catalog_entries_by_digest_in_tx(
        &self,
        transaction: &mut Transaction<'_, Sqlite>,
        manifest_digest: &str,
    ) -> McpPlatformResult<Vec<GovernedCatalogEntryRecord>> {
        let query = format!(
            "{GOVERNED_CATALOG_ENTRY_PROJECTION}\nWHERE e.manifest_digest = ?\nORDER BY e.source_id, e.mcp_id, e.version, e.entry_id"
        );
        let rows = sqlx::query(&query)
            .bind(manifest_digest)
            .fetch_all(&mut **transaction)
            .await
            .map_err(map_sqlx)?;
        let entries = rows
            .iter()
            .map(decode_governed_catalog_entry_row)
            .collect::<McpPlatformResult<Vec<_>>>()?;
        let governed_entry_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM governed_catalog_entries WHERE manifest_digest = ?",
        )
        .bind(manifest_digest)
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        if entries.len() != governed_entry_count as usize {
            return Err(integrity_error());
        }
        if entries.is_empty() {
            return Err(not_found());
        }
        Ok(entries)
    }

    async fn get_governed_catalog_entry_by_source_and_digest_in_tx(
        &self,
        transaction: &mut Transaction<'_, Sqlite>,
        source_id: &str,
        manifest_digest: &str,
    ) -> McpPlatformResult<GovernedCatalogEntryRecord> {
        let query = format!(
            "{GOVERNED_CATALOG_ENTRY_PROJECTION}\nWHERE e.source_id = ? AND e.manifest_digest = ?"
        );
        let row = sqlx::query(&query)
            .bind(source_id)
            .bind(manifest_digest)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(map_sqlx)?;
        let Some(row) = row else {
            let entry_exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM governed_catalog_entries WHERE source_id = ? AND manifest_digest = ?)",
            )
            .bind(source_id)
            .bind(manifest_digest)
            .fetch_one(&mut **transaction)
            .await
            .map_err(map_sqlx)?;
            return if entry_exists {
                Err(integrity_error())
            } else {
                Err(not_found())
            };
        };
        decode_governed_catalog_entry_row(&row)
    }

    pub(crate) async fn source_catalog_document_provenance(
        &self,
        source_id: &str,
    ) -> McpPlatformResult<StoredSourceCatalogDocumentProvenance> {
        let mut transaction = self.begin_verified_read_snapshot().await?;
        let result = self
            .source_catalog_document_provenance_in_tx(&mut transaction, source_id)
            .await;
        finish_verified_read_snapshot(transaction, result).await
    }

    async fn source_catalog_document_provenance_in_tx(
        &self,
        transaction: &mut Transaction<'_, Sqlite>,
        source_id: &str,
    ) -> McpPlatformResult<StoredSourceCatalogDocumentProvenance> {
        let row = sqlx::query(
            r#"SELECT
                    d.source_name, d.document_digest, d.canonical_digest, d.signed_digest,
                    d.binding_digest
               FROM source_catalog_documents d
               WHERE d.source_id = ?"#,
        )
        .bind(source_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let signature_rows = sqlx::query(
            r#"SELECT kid
               FROM source_signatures
               WHERE source_id = ?
               ORDER BY signature_order"#,
        )
        .bind(source_id)
        .fetch_all(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        Ok(StoredSourceCatalogDocumentProvenance {
            source_name: row.try_get("source_name").map_err(map_sqlx)?,
            document_digest: row.try_get("document_digest").map_err(map_sqlx)?,
            canonical_digest: row.try_get("canonical_digest").map_err(map_sqlx)?,
            signed_digest: row.try_get("signed_digest").map_err(map_sqlx)?,
            binding_digest: row.try_get("binding_digest").map_err(map_sqlx)?,
            signature_kids: signature_rows
                .into_iter()
                .map(|signature| signature.try_get("kid").map_err(map_sqlx))
                .collect::<McpPlatformResult<Vec<String>>>()?,
        })
    }

    pub(crate) async fn source_catalog_release_matches(
        &self,
        source_id: &str,
        release_id: &str,
        mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<bool> {
        let mut transaction = self.begin_verified_read_snapshot().await?;
        let result = self
            .source_catalog_release_matches_in_tx(
                &mut transaction,
                source_id,
                release_id,
                mcp_id,
                version,
            )
            .await;
        finish_verified_read_snapshot(transaction, result).await
    }

    async fn source_catalog_release_matches_in_tx(
        &self,
        transaction: &mut Transaction<'_, Sqlite>,
        source_id: &str,
        release_id: &str,
        mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<bool> {
        let exists = sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                    SELECT 1
                    FROM source_releases release
                    WHERE release.source_id = ?
                      AND release.release_id = ?
                      AND release.mcp_id = ?
                      AND release.version = ?
                      AND NOT EXISTS (
                          SELECT 1
                          FROM source_revocations revocation
                          WHERE revocation.source_id = release.source_id
                            AND revocation.release_id = release.release_id
                      )
               )"#,
        )
        .bind(source_id)
        .bind(release_id)
        .bind(mcp_id)
        .bind(version)
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        Ok(exists)
    }

    pub(crate) async fn store_verified_source_document(
        &self,
        document: &crate::verified_source_catalog::VerifiedSourceDocument,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let dao = crate::verified_source_catalog::SourceCatalogDao::new(self.pool.clone());
        dao.store_verified_document(document, now_ms)
            .await
            .map_err(|_| repository_unavailable())
    }

    pub(crate) async fn create_managed_mcp(
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
        .execute(&mut **tx)
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

    pub(crate) async fn update_managed_state(
        &self,
        managed_mcp_id: &str,
        expected_revision: i64,
        state: &ManagedMcpState,
        now_ms: i64,
    ) -> McpPlatformResult<ManagedMcpRecord> {
        let current = self.get_managed_mcp(managed_mcp_id).await?;
        let update = ManagedMcpStateUpdate::preserving_default_enabled(
            state,
            current.state.default_enabled,
        )?;
        self.apply_managed_state_update(managed_mcp_id, expected_revision, &update, now_ms)
            .await
    }

    pub(crate) async fn apply_managed_state_update(
        &self,
        managed_mcp_id: &str,
        expected_revision: i64,
        state: &ManagedMcpStateUpdate,
        now_ms: i64,
    ) -> McpPlatformResult<ManagedMcpRecord> {
        let mut tx = self.begin_immediate().await?;
        let row =
            sqlx::query("SELECT revision, state_json FROM managed_mcps WHERE managed_mcp_id = ?")
                .bind(managed_mcp_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx)?
                .ok_or_else(not_found)?;
        let revision: i64 = row.try_get("revision").map_err(map_sqlx)?;
        if revision != expected_revision {
            return Err(revision_conflict());
        }
        let current =
            decode::<ManagedMcpState>(&row.try_get::<String, _>("state_json").map_err(map_sqlx)?)?;
        let next = state.apply_to(current.default_enabled);
        sqlx::query(
            "UPDATE managed_mcps SET state_json = ?, revision = revision + 1, updated_at_ms = ? WHERE managed_mcp_id = ?",
        )
        .bind(encode(&next)?)
        .bind(now_ms)
        .bind(managed_mcp_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_managed_mcp(managed_mcp_id).await
    }

    pub(crate) async fn save_managed_version(
        &self,
        managed_mcp_id: &str,
        version: &ManagedVersionRecord,
    ) -> McpPlatformResult<InsertOutcome> {
        let mut tx = self.begin_immediate().await?;
        if let Some(row) = sqlx::query(
            r#"SELECT version, manifest_digest, installation_root, verified, active,
                adapter_evidence_json, materialized_tree_digest, supply_chain_evidence_json,
                created_at_ms FROM managed_versions
                WHERE managed_mcp_id = ? AND version = ?"#,
        )
        .bind(managed_mcp_id)
        .bind(&version.version)
        .fetch_optional(&mut **tx)
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
                adapter_evidence_json, materialized_tree_digest, supply_chain_evidence_json,
                created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(managed_mcp_id)
        .bind(&version.version)
        .bind(&version.manifest_digest)
        .bind(&version.installation_root)
        .bind(version.verified)
        .bind(version.active)
        .bind(encode_optional(version.adapter_evidence.as_ref())?)
        .bind(&version.materialized_tree_digest)
        .bind(encode_optional(version.supply_chain_evidence.as_ref())?)
        .bind(version.created_at_ms)
        .execute(&mut **tx)
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
                adapter_evidence_json, materialized_tree_digest, supply_chain_evidence_json,
                created_at_ms FROM managed_versions
                WHERE managed_mcp_id = ? ORDER BY version"#,
        )
        .bind(managed_mcp_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        rows.iter().map(decode_managed_version_row).collect()
    }

    pub(crate) async fn put_connection_projection(
        &self,
        _managed_mcp_id: &str,
        _link_key: &str,
        _projection: &crate::mcp_platform::plan::ConnectionProjection,
        _expected_revision: Option<i64>,
        _now_ms: i64,
    ) -> McpPlatformResult<ConnectionProjectionRecord> {
        Err(error(
            McpPlatformErrorCode::ProjectionConflict,
            "managed connection projection requires an authorized task writer",
        ))
    }

    pub async fn get_connection_projection(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ConnectionProjectionRecord> {
        let mut tx = self.begin_immediate().await?;
        let row = sqlx::query(
            r#"SELECT managed_mcp_id, link_key, projection_json, revision, updated_at_ms,
                plan_id, manifest_digest, owner_task_id, projection_digest
                FROM connection_projections WHERE managed_mcp_id = ?"#,
        )
        .bind(managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let projection_json = row
            .try_get::<String, _>("projection_json")
            .map_err(map_sqlx)?;
        let projection_digest: String = row.try_get("projection_digest").map_err(map_sqlx)?;
        let plan_id: Option<String> = row.try_get("plan_id").map_err(map_sqlx)?;
        let manifest_digest: Option<String> = row.try_get("manifest_digest").map_err(map_sqlx)?;
        let owner_task_id: Option<String> = row.try_get("owner_task_id").map_err(map_sqlx)?;
        let legacy = plan_id.is_none() && manifest_digest.is_none() && owner_task_id.is_none();
        if projection_digest.is_empty() {
            return Err(integrity_error());
        }
        let projection = crate::mcp_platform::decode_verified_projection_config(
            &projection_json,
            &projection_digest,
        )?;
        let record = ConnectionProjectionRecord {
            managed_mcp_id: row.try_get("managed_mcp_id").map_err(map_sqlx)?,
            link_key: row.try_get("link_key").map_err(map_sqlx)?,
            projection,
            revision: row.try_get("revision").map_err(map_sqlx)?,
            updated_at_ms: row.try_get("updated_at_ms").map_err(map_sqlx)?,
            plan_id,
            manifest_digest,
            owner_task_id,
            projection_digest,
        };
        let mac = sqlx::query_scalar::<_, String>(
            "SELECT mac FROM managed_projection_integrity WHERE managed_mcp_id=?",
        )
        .bind(managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(integrity_error)?;
        self.integrity_signer.verify(
            "managed-projection",
            &managed_projection_integrity_payload(
                &record.managed_mcp_id,
                &record.link_key,
                &record.projection_digest,
                record.revision,
                record.plan_id.as_deref(),
                record.manifest_digest.as_deref(),
                record.owner_task_id.as_deref(),
            ),
            &mac,
        )?;
        if !legacy {
            let (Some(plan_id), Some(manifest_digest), Some(owner_task_id)) = (
                record.plan_id.as_deref(),
                record.manifest_digest.as_deref(),
                record.owner_task_id.as_deref(),
            ) else {
                return Err(integrity_error());
            };
            let managed = self.get_managed_mcp(managed_mcp_id).await?;
            let plan = self.get_plan(plan_id).await?;
            let owner_task = self.get_task(owner_task_id).await?;
            let scope = plan.target.installation_scope.as_deref().unwrap_or("user");
            let expected_managed_mcp_id = crate::mcp_platform::task_runner::stable_managed_mcp_id(
                plan.plan.manifest_id(),
                scope,
            );
            let expected_link_key = format!(
                "managed_mcp_{}",
                expected_managed_mcp_id
                    .strip_prefix("managed_")
                    .ok_or_else(integrity_error)?
            );
            if plan.plan.manifest_digest() != manifest_digest
                || plan.target.mcp_id != managed.mcp_id
                || plan
                    .target
                    .managed_mcp_id
                    .as_deref()
                    .is_some_and(|id| id != managed_mcp_id)
                || expected_managed_mcp_id != managed_mcp_id
                || expected_link_key != record.link_key
                || plan.plan.connection_projection() != &record.projection
                || owner_task.plan_id != plan_id
                || owner_task.plan_digest != plan.plan.plan_digest()
                || owner_task.operation != plan.plan.operation().into()
            {
                return Err(integrity_error());
            }
            lifecycle_repository::verify_projection_writer_binding(
                &mut tx,
                self.integrity_signer.as_ref(),
                managed_mcp_id,
                owner_task_id,
                plan_id,
                &record.link_key,
                manifest_digest,
                &record.projection_digest,
            )
            .await?;
        }
        tx.rollback().await?;
        Ok(record)
    }

    pub(crate) async fn save_plan(&self, input: SavePlan<'_>) -> McpPlatformResult<PlanRecord> {
        input.plan.verify_integrity()?;
        if input.target.source_context != input.plan.source_context().cloned() {
            return Err(integrity_error());
        }
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
        if matches!(
            input.confirmation_evidence,
            crate::mcp_platform::repository::ConfirmationEvidence::Confirmed { actor, .. }
                if actor != input.actor
        ) {
            return Err(integrity_error());
        }
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
        .fetch_optional(&mut **tx)
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
        .fetch_one(&mut **tx)
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
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_plan(input.plan_id).await
    }

    #[cfg(test)]
    pub(crate) async fn rewrite_plan_target_for_testing(
        &self,
        plan_id: &str,
        target: &PlanTarget,
    ) -> McpPlatformResult<PlanRecord> {
        let mut tx = self.begin_immediate().await?;
        let existing = fetch_valid_plan(&mut tx, plan_id).await?;
        if target.mcp_id != existing.plan.manifest_id()
            || target.version != existing.plan.manifest_version()
            || target.source_context != existing.plan.source_context().cloned()
        {
            return Err(integrity_error());
        }
        let envelope_digest = plan_envelope_digest_for_save(&SavePlan {
            plan_id: &existing.plan_id,
            idempotency_key: &existing.idempotency_key,
            plan: &existing.plan,
            target,
            policy_evidence: &existing.policy_evidence,
            confirmation_evidence: &existing.confirmation_evidence,
            expires_at_ms: existing.expires_at_ms,
            created_at_ms: existing.created_at_ms,
            actor: &existing.actor,
        })?;
        let updated = sqlx::query(
            "UPDATE install_plans SET target_json = ?, envelope_digest = ? WHERE plan_id = ?",
        )
        .bind(encode(target)?)
        .bind(&envelope_digest)
        .bind(plan_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if updated.rows_affected() != 1 {
            return Err(not_found());
        }
        tx.commit().await.map_err(map_sqlx)?;
        self.get_plan(plan_id).await
    }

    #[cfg(test)]
    pub(crate) async fn rewrite_managed_active_manifest_digest_for_testing(
        &self,
        managed_mcp_id: &str,
        active_manifest_digest: &str,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let updated = sqlx::query(
            "UPDATE managed_mcps SET active_manifest_digest = ? WHERE managed_mcp_id = ?",
        )
        .bind(active_manifest_digest)
        .bind(managed_mcp_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if updated.rows_affected() != 1 {
            return Err(not_found());
        }
        tx.commit().await.map_err(map_sqlx)
    }

    #[cfg(test)]
    pub(crate) async fn rewrite_projection_manifest_digest_for_testing(
        &self,
        managed_mcp_id: &str,
        manifest_digest: &str,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let row = sqlx::query(
            "SELECT link_key, projection_digest, revision, plan_id, owner_task_id FROM connection_projections WHERE managed_mcp_id = ?",
        )
        .bind(managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let link_key: String = row.try_get("link_key").map_err(map_sqlx)?;
        let projection_digest: String = row.try_get("projection_digest").map_err(map_sqlx)?;
        let revision: i64 = row.try_get("revision").map_err(map_sqlx)?;
        let plan_id: Option<String> = row.try_get("plan_id").map_err(map_sqlx)?;
        let owner_task_id: Option<String> = row.try_get("owner_task_id").map_err(map_sqlx)?;
        let mac = self.integrity_signer.sign(
            "managed-projection",
            &managed_projection_integrity_payload(
                managed_mcp_id,
                &link_key,
                &projection_digest,
                revision,
                plan_id.as_deref(),
                Some(manifest_digest),
                owner_task_id.as_deref(),
            ),
        )?;
        let updated = sqlx::query(
            "UPDATE connection_projections SET manifest_digest = ? WHERE managed_mcp_id = ?",
        )
        .bind(manifest_digest)
        .bind(managed_mcp_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if updated.rows_affected() != 1 {
            return Err(not_found());
        }
        let mac_updated =
            sqlx::query("UPDATE managed_projection_integrity SET mac = ? WHERE managed_mcp_id = ?")
                .bind(mac)
                .bind(managed_mcp_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
        if mac_updated.rows_affected() != 1 {
            return Err(integrity_error());
        }
        tx.commit().await.map_err(map_sqlx)
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
        let (manifest, _) = self.resolve_manifest_for_plan(&record).await?;
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

    pub(super) async fn resolve_source_context_for_digest(
        &self,
        digest: &str,
        fallback_metadata: Option<&ManifestSourceMetadata>,
    ) -> McpPlatformResult<ManifestSourceContext> {
        match self.list_governed_catalog_entries_by_digest(digest).await {
            Ok(entries) => {
                let mut source_ids = entries
                    .into_iter()
                    .map(|entry| entry.source.source_id)
                    .collect::<Vec<_>>();
                source_ids.sort();
                source_ids.dedup();
                match source_ids.as_slice() {
                    [source_id] => Ok(ManifestSourceContext::GovernedCatalog {
                        source_id: source_id.clone(),
                    }),
                    [] => Err(not_found()),
                    _ => Err(error(
                        McpPlatformErrorCode::InvalidRequest,
                        "manifest digest requires an explicit source selection",
                    )),
                }
            }
            Err(error_value) if error_value.code() == McpPlatformErrorCode::NotFound => {
                Ok(ManifestSourceContext::PersistedManifest {
                    source_id: persisted_source_id(fallback_metadata.ok_or_else(integrity_error)?)?,
                })
            }
            Err(error_value) => Err(error_value),
        }
    }

    pub(crate) async fn resolve_manifest_for_plan(
        &self,
        plan: &PlanRecord,
    ) -> McpPlatformResult<(ManifestRecord, ManifestSourceContext)> {
        if plan.target.source_context != plan.plan.source_context().cloned() {
            return Err(integrity_error());
        }
        if let Some(ManifestSourceContext::GovernedCatalog { source_id }) =
            plan.target.source_context.as_ref()
        {
            let entry = self
                .get_governed_catalog_entry(source_id, &plan.target.mcp_id, &plan.target.version)
                .await?;
            if entry.manifest.verified.digest() != plan.plan.manifest_digest()
                || entry.manifest.verified.manifest().id != plan.plan.manifest_id()
                || entry.manifest.verified.manifest().version.as_str()
                    != plan.plan.manifest_version()
            {
                return Err(integrity_error());
            }
            return Ok((
                entry.manifest,
                ManifestSourceContext::GovernedCatalog {
                    source_id: source_id.clone(),
                },
            ));
        }

        let fallback_manifest = match plan.target.source_context.as_ref() {
            Some(_) => None,
            None => {
                let manifest = self.get_manifest(plan.plan.manifest_digest()).await?;
                if matches!(
                    &manifest.source_metadata.source_ref,
                    crate::mcp_platform::manifest::SourceRef::HttpsManifestUrl { .. }
                ) {
                    return Err(integrity_error());
                }
                Some(manifest)
            }
        };
        let source_context = match plan.target.source_context.as_ref() {
            Some(source_context) => source_context.clone(),
            None => {
                let fallback_metadata = fallback_manifest
                    .as_ref()
                    .map(|manifest| &manifest.source_metadata);
                self.resolve_source_context_for_digest(
                    plan.plan.manifest_digest(),
                    fallback_metadata,
                )
                .await?
            }
        };
        if let ManifestSourceContext::HttpsProvision { binding } = &source_context {
            if plan.plan.source_context() != Some(&source_context)
                || binding.parsed_digest != plan.plan.manifest_digest()
            {
                return Err(integrity_error());
            }
            let manifest = self.get_manifest_for_https_source_binding(binding).await?;
            if manifest.verified.digest() != plan.plan.manifest_digest()
                || manifest.verified.manifest().id != plan.plan.manifest_id()
                || manifest.verified.manifest().version.as_str() != plan.plan.manifest_version()
            {
                return Err(integrity_error());
            }
            return Ok((manifest, source_context));
        }
        let manifest = match (&source_context, fallback_manifest) {
            (ManifestSourceContext::PersistedManifest { source_id }, Some(manifest))
                if persisted_source_id(&manifest.source_metadata)? == *source_id =>
            {
                manifest
            }
            _ => {
                self.get_manifest_with_source_context(plan.plan.manifest_digest(), &source_context)
                    .await?
            }
        };
        Ok((manifest, source_context))
    }

    async fn begin_immediate(&self) -> McpPlatformResult<AnchoredTransaction<'_>> {
        let process_guard = self.integrity_transaction_lock.clone().lock_owned().await;
        let database_guard = self
            .database_lock_path
            .as_deref()
            .map(DatabaseLockGuard::acquire_persistent)
            .transpose()?;
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(map_sqlx)?;
        let checkpoint =
            verify_integrity_transaction(&mut transaction, self.integrity_signer.as_ref()).await?;
        Ok(AnchoredTransaction {
            transaction: Some(transaction),
            signer: self.integrity_signer.clone(),
            checkpoint,
            _process_guard: process_guard,
            _database_guard: database_guard,
        })
    }

    pub(super) async fn begin_verified_read_snapshot(
        &self,
    ) -> McpPlatformResult<Transaction<'_, Sqlite>> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx)?;
        verify_integrity_transaction(&mut transaction, self.integrity_signer.as_ref()).await?;
        Ok(transaction)
    }
}

struct FileBackedBootstrapFailure {
    error: crate::mcp_platform::error::McpPlatformError,
    cleanup_target: FileBackedBootstrapCleanupTarget,
}

enum FileBackedBootstrapCleanupTarget {
    Staged(PathBuf),
    Final(PathBuf),
    StagedAndCreatedFinal {
        staged: PathBuf,
        final_path: PathBuf,
    },
}

impl FileBackedBootstrapFailure {
    fn staged(error: crate::mcp_platform::error::McpPlatformError, path: PathBuf) -> Self {
        Self {
            error,
            cleanup_target: FileBackedBootstrapCleanupTarget::Staged(path),
        }
    }

    fn final_path(error: crate::mcp_platform::error::McpPlatformError, path: PathBuf) -> Self {
        Self {
            error,
            cleanup_target: FileBackedBootstrapCleanupTarget::Final(path),
        }
    }

    fn created_final(
        error: crate::mcp_platform::error::McpPlatformError,
        staged: PathBuf,
        final_path: PathBuf,
    ) -> Self {
        Self {
            error,
            cleanup_target: FileBackedBootstrapCleanupTarget::StagedAndCreatedFinal {
                staged,
                final_path,
            },
        }
    }

    fn cleanup_paths(&self) -> Vec<PathBuf> {
        match &self.cleanup_target {
            FileBackedBootstrapCleanupTarget::Staged(path) => {
                sqlite_database_and_sidecar_paths(path)
            }
            FileBackedBootstrapCleanupTarget::Final(path) => {
                sqlite_database_and_sidecar_paths(path)
            }
            FileBackedBootstrapCleanupTarget::StagedAndCreatedFinal { staged, final_path } => {
                let mut paths = sqlite_database_and_sidecar_paths(final_path);
                paths.extend(sqlite_database_and_sidecar_paths(staged));
                paths
            }
        }
    }

    fn cleanup_error(&self) -> crate::mcp_platform::error::McpPlatformError {
        match &self.cleanup_target {
            FileBackedBootstrapCleanupTarget::Staged(_) => bootstrap_cleanup_failed(),
            FileBackedBootstrapCleanupTarget::Final(_)
            | FileBackedBootstrapCleanupTarget::StagedAndCreatedFinal { .. } => {
                activated_database_cleanup_failed()
            }
        }
    }

    fn created_final_cleanup(
        &self,
    ) -> Option<(Vec<PathBuf>, crate::mcp_platform::error::McpPlatformError)> {
        match &self.cleanup_target {
            FileBackedBootstrapCleanupTarget::StagedAndCreatedFinal { .. } => {
                Some((self.cleanup_paths(), self.cleanup_error()))
            }
            FileBackedBootstrapCleanupTarget::Staged(_)
            | FileBackedBootstrapCleanupTarget::Final(_) => None,
        }
    }

    fn into_error(self) -> crate::mcp_platform::error::McpPlatformError {
        self.error
    }
}

struct AnchoredTransaction<'a> {
    transaction: Option<Transaction<'a, Sqlite>>,
    signer: Arc<dyn IntegritySigner>,
    checkpoint: integrity::AnchorCheckpoint,
    _process_guard: OwnedMutexGuard<()>,
    _database_guard: Option<DatabaseLockGuard>,
}

impl<'a> Deref for AnchoredTransaction<'a> {
    type Target = Transaction<'a, Sqlite>;

    fn deref(&self) -> &Self::Target {
        self.transaction
            .as_ref()
            .expect("anchored transaction already completed")
    }
}

impl<'a> DerefMut for AnchoredTransaction<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.transaction
            .as_mut()
            .expect("anchored transaction already completed")
    }
}

impl AnchoredTransaction<'_> {
    async fn commit(mut self) -> McpPlatformResult<()> {
        self.commit_with_checkpoint().await.map(|_| ())
    }

    async fn commit_with_checkpoint(mut self) -> McpPlatformResult<integrity::AnchorCheckpoint> {
        let transaction = self
            .transaction
            .as_mut()
            .expect("anchored transaction already completed");
        let state_digest = security_state_digest(transaction).await?;
        let next = next_checkpoint(&self.checkpoint, &state_digest);
        let commit_mac = sign_checkpoint(
            self.signer.as_ref(),
            &next,
            &self.checkpoint.root,
            &state_digest,
        )?;
        sqlx::query(
            "INSERT INTO integrity_commits(sequence,instance_id,key_epoch,provider_id,parent_root,state_digest,root,commit_mac) VALUES (?,?,?,?,?,?,?,?)",
        )
        .bind(i64::try_from(next.sequence).map_err(|_| integrity::integrity_recovery_required())?)
        .bind(&next.identity.instance_id)
        .bind(i64::try_from(next.identity.key_epoch).map_err(|_| integrity::integrity_recovery_required())?)
        .bind(&next.identity.provider_id)
        .bind(&self.checkpoint.root)
        .bind(&state_digest)
        .bind(&next.root)
        .bind(&commit_mac)
        .execute(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        let updated = sqlx::query(
            "UPDATE integrity_metadata SET sequence=?,root=?,state_digest=?,commit_mac=?,status='active' WHERE singleton=1 AND instance_id=? AND key_epoch=? AND provider_id=? AND sequence=? AND root=? AND status='active'",
        )
        .bind(i64::try_from(next.sequence).map_err(|_| integrity::integrity_recovery_required())?)
        .bind(&next.root)
        .bind(&state_digest)
        .bind(&commit_mac)
        .bind(&next.identity.instance_id)
        .bind(i64::try_from(next.identity.key_epoch).map_err(|_| integrity::integrity_recovery_required())?)
        .bind(&next.identity.provider_id)
        .bind(i64::try_from(self.checkpoint.sequence).map_err(|_| integrity::integrity_recovery_required())?)
        .bind(&self.checkpoint.root)
        .execute(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        if updated.rows_affected() != 1 {
            return Err(integrity::integrity_recovery_required());
        }
        self.transaction
            .take()
            .expect("anchored transaction already completed")
            .commit()
            .await
            .map_err(map_sqlx)?;
        self.signer.publish(Some(&self.checkpoint), &next)?;
        Ok(next)
    }

    #[allow(dead_code)]
    async fn rollback(mut self) -> McpPlatformResult<()> {
        self.transaction
            .take()
            .expect("anchored transaction already completed")
            .rollback()
            .await
            .map_err(map_sqlx)
    }
}

impl SqliteMcpPlatformRepository {
    pub(crate) async fn confirm_source_provisioning(
        &self,
        input: ConfirmSourceProvisioning,
    ) -> McpPlatformResult<SourceProvisioningAuditRecord> {
        let mut tx = self.begin_immediate().await?;
        let parsed = crate::mcp_platform::source_provisioning::parse_source_provisioning_snapshot(
            &input.frozen,
        )
        .map_err(|_| integrity_error())?;
        validate_source_provisioning_commit_input(&input, &parsed)?;
        reject_existing_source_provisioning_state_in_tx(&mut tx, &input, &parsed).await?;

        let document = input
            .import
            .verified_source_document
            .as_ref()
            .ok_or_else(integrity_error)?;
        crate::verified_source_catalog::dao::store_verified_document_tx(
            &mut tx,
            document,
            input.occurred_at_ms,
        )
        .await
        .map_err(|_| integrity_error())?;
        save_governed_catalog_import_records_in_tx(&mut tx, &input.import, true).await?;

        sqlx::query(
            r#"INSERT INTO governed_source_trust_pins(
                    source_id, verified_source_id, trust_basis, root_digest, endpoint,
                    source_document_digest, descriptor_digest, created_at_ms
               ) VALUES (?, ?, 'user_pin', ?, ?, ?, ?, ?)"#,
        )
        .bind(&input.import.source.source_id)
        .bind(&parsed.descriptor.source_id)
        .bind(&parsed.descriptor.root_digest)
        .bind(&parsed.descriptor.endpoint)
        .bind(&parsed.source_document.digests.document_digest)
        .bind(&parsed.descriptor.descriptor_digest)
        .bind(input.occurred_at_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;

        sqlx::query(
            r#"INSERT INTO governed_source_refresh_registrations(
                    source_id, transport_kind, endpoint, created_at_ms, updated_at_ms,
                    last_attempted_at_ms, last_refreshed_at_ms, last_result,
                    last_document_digest, last_error_code
               ) VALUES (?, ?, ?, ?, ?, NULL, NULL, 'idle', NULL, NULL)"#,
        )
        .bind(&input.import.source.source_id)
        .bind(parsed.descriptor.transport_kind.as_str())
        .bind(&parsed.descriptor.endpoint)
        .bind(input.occurred_at_ms)
        .bind(input.occurred_at_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;

        let anchor = parsed.source_document.trust_anchor();
        sqlx::query(
            r#"INSERT INTO governed_source_refresh_trust_anchors(
                    source_id, verified_source_id, endpoint, root_digest,
                    anchor_document_digest, anchor_document_bytes, created_at_ms
               ) VALUES (?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&input.import.source.source_id)
        .bind(&anchor.source_id)
        .bind(&parsed.descriptor.endpoint)
        .bind(&anchor.root_digest)
        .bind(&parsed.source_document.digests.document_digest)
        .bind(&parsed.source_document.raw_bytes)
        .bind(input.occurred_at_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;

        sqlx::query(
            r#"INSERT INTO governed_source_provision_audits(
                    provision_id, source_id, document_digest, root_digest, descriptor_digest,
                    trust_basis, actor, correlation_id, occurred_at_ms
               ) VALUES (?, ?, ?, ?, ?, 'user_pin', ?, ?, ?)"#,
        )
        .bind(&input.provision_id)
        .bind(&input.import.source.source_id)
        .bind(&parsed.source_document.digests.document_digest)
        .bind(&parsed.descriptor.root_digest)
        .bind(&parsed.descriptor.descriptor_digest)
        .bind(&input.actor)
        .bind(&input.correlation_id)
        .bind(input.occurred_at_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;

        let result = SourceProvisioningAuditRecord {
            provision_id: input.provision_id,
            source_id: input.import.source.source_id,
            document_digest: parsed.source_document.digests.document_digest,
            root_digest: parsed.descriptor.root_digest,
            descriptor_digest: parsed.descriptor.descriptor_digest,
            trust_basis: GovernedSourceTrustBasis::UserPin,
            actor: input.actor,
            correlation_id: input.correlation_id,
            occurred_at_ms: input.occurred_at_ms,
        };
        tx.commit().await?;
        Ok(result)
    }

    pub(crate) async fn get_governed_source_trust_pin(
        &self,
        source_id: &str,
    ) -> McpPlatformResult<GovernedSourceTrustPinRecord> {
        let row = sqlx::query(
            r#"SELECT source_id, verified_source_id, trust_basis, root_digest, endpoint,
                      source_document_digest, descriptor_digest, created_at_ms
                 FROM governed_source_trust_pins WHERE source_id = ?"#,
        )
        .bind(source_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        decode_governed_source_trust_pin_row(&row)
    }

    pub(crate) async fn has_governed_source_trust_pin(
        &self,
        source_id: &str,
    ) -> McpPlatformResult<bool> {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM governed_source_trust_pins WHERE source_id = ?)",
        )
        .bind(source_id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx)
    }

    pub async fn save_governed_source_refresh_registration(
        &self,
        input: SaveGovernedSourceRefreshRegistration<'_>,
    ) -> McpPlatformResult<GovernedSourceRefreshRegistrationRecord> {
        let mut tx = self.begin_immediate().await?;
        let source_row =
            sqlx::query("SELECT source_ref_json FROM governed_catalog_sources WHERE source_id = ?")
                .bind(input.source_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx)?
                .ok_or_else(not_found)?;
        let source_ref: SourceRef = decode(
            source_row
                .try_get::<&str, _>("source_ref_json")
                .map_err(map_sqlx)?,
        )?;
        let SourceRef::VerifiedSourceCatalog {
            source_id: verified_source_id,
        } = source_ref
        else {
            return Err(error(
                McpPlatformErrorCode::InvalidRequest,
                "governed source refresh requires a verified source catalog",
            ));
        };
        let pin_row = sqlx::query(
            r#"SELECT source_id, verified_source_id, trust_basis, root_digest, endpoint,
                      source_document_digest, descriptor_digest, created_at_ms
                 FROM governed_source_trust_pins WHERE source_id = ?"#,
        )
        .bind(input.source_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(|| {
            error(
                McpPlatformErrorCode::OperationNotSupported,
                "governed source refresh registration requires trusted source provisioning",
            )
        })?;
        let pin = decode_governed_source_trust_pin_row(&pin_row)?;
        if pin.trust_basis != GovernedSourceTrustBasis::UserPin
            || pin.source_id != input.source_id
            || pin.verified_source_id != verified_source_id
            || pin.endpoint != input.endpoint
        {
            return Err(integrity_error());
        }
        let existing = sqlx::query(
            r#"SELECT source_id, transport_kind, endpoint, created_at_ms, updated_at_ms,
                    last_attempted_at_ms, last_refreshed_at_ms, last_result,
                    last_document_digest, last_error_code
               FROM governed_source_refresh_registrations
               WHERE source_id = ?"#,
        )
        .bind(input.source_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if let Some(existing) = existing {
            let existing_record = decode_governed_source_refresh_registration_row(&existing)?;
            let anchor_exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM governed_source_refresh_trust_anchors WHERE source_id = ?)",
            )
            .bind(input.source_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(map_sqlx)?;
            tx.rollback().await?;
            if existing_record.endpoint != input.endpoint {
                return Err(error(
                    McpPlatformErrorCode::InvalidRequest,
                    "governed source refresh endpoint is immutable after provisioning",
                ));
            }
            if !anchor_exists {
                return Err(error(
                    McpPlatformErrorCode::OperationNotSupported,
                    "governed source refresh registration requires trusted re-provisioning",
                ));
            }
            return Ok(existing_record);
        }

        let raw_document = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT raw_bytes FROM source_catalog_documents WHERE source_id = ?",
        )
        .bind(&verified_source_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(integrity_error)?;
        let trusted_document = crate::verified_source_catalog::parse_signed_envelope(&raw_document)
            .map_err(|_| integrity_error())?;
        if trusted_document.envelope.payload.source_id != verified_source_id {
            return Err(integrity_error());
        }
        let anchor = trusted_document.trust_anchor();
        if pin.root_digest != anchor.root_digest
            || pin.source_document_digest != trusted_document.digests.document_digest
        {
            return Err(integrity_error());
        }
        sqlx::query(
            r#"INSERT INTO governed_source_refresh_registrations(
                    source_id, transport_kind, endpoint, created_at_ms, updated_at_ms,
                    last_attempted_at_ms, last_refreshed_at_ms, last_result,
                    last_document_digest, last_error_code
               ) VALUES (?, ?, ?, ?, ?, NULL, NULL, 'idle', NULL, NULL)"#,
        )
        .bind(input.source_id)
        .bind(input.transport_kind.as_str())
        .bind(input.endpoint)
        .bind(input.now_ms)
        .bind(input.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            r#"INSERT INTO governed_source_refresh_trust_anchors(
                    source_id, verified_source_id, endpoint, root_digest,
                    anchor_document_digest, anchor_document_bytes, created_at_ms
               ) VALUES (?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(input.source_id)
        .bind(&anchor.source_id)
        .bind(input.endpoint)
        .bind(&anchor.root_digest)
        .bind(&trusted_document.digests.document_digest)
        .bind(&trusted_document.raw_bytes)
        .bind(input.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_governed_source_refresh_registration(input.source_id)
            .await
    }

    pub async fn get_governed_source_refresh_registration(
        &self,
        source_id: &str,
    ) -> McpPlatformResult<GovernedSourceRefreshRegistrationRecord> {
        let row = sqlx::query(
            r#"SELECT
                    source_id, transport_kind, endpoint, created_at_ms, updated_at_ms,
                    last_attempted_at_ms, last_refreshed_at_ms, last_result,
                    last_document_digest, last_error_code
               FROM governed_source_refresh_registrations
               WHERE source_id = ?"#,
        )
        .bind(source_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        decode_governed_source_refresh_registration_row(&row)
    }

    pub async fn get_governed_source_refresh_trust_anchor(
        &self,
        source_id: &str,
    ) -> McpPlatformResult<GovernedSourceRefreshTrustAnchorRecord> {
        let row = sqlx::query(
            r#"SELECT
                    registration.source_id,
                    registration.endpoint AS registration_endpoint,
                    anchor.verified_source_id,
                    anchor.endpoint AS anchor_endpoint,
                    anchor.root_digest,
                    anchor.anchor_document_digest,
                    anchor.anchor_document_bytes,
                    anchor.created_at_ms
               FROM governed_source_refresh_registrations registration
               JOIN governed_source_refresh_trust_anchors anchor
                 ON anchor.source_id = registration.source_id
               WHERE registration.source_id = ?"#,
        )
        .bind(source_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(|| {
            error(
                McpPlatformErrorCode::OperationNotSupported,
                "governed source refresh registration has no trusted anchor",
            )
        })?;
        decode_governed_source_refresh_trust_anchor_row(&row)
    }

    pub(crate) async fn has_governed_source_refresh_trust_anchor(
        &self,
        source_id: &str,
    ) -> McpPlatformResult<bool> {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM governed_source_refresh_trust_anchors WHERE source_id = ?)",
        )
        .bind(source_id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx)
    }

    pub async fn apply_governed_source_refresh(
        &self,
        input: ApplyGovernedSourceRefresh,
    ) -> McpPlatformResult<GovernedSourceRefreshCommitOutcome> {
        let Some(document) = input.import.verified_source_document.as_ref() else {
            return Err(error(
                McpPlatformErrorCode::InvalidRequest,
                "governed source refresh requires a verified source document",
            ));
        };
        if input.import.source.source_id != input.source_id
            || input.import.document.source_id != input.source_id
        {
            return Err(error(
                McpPlatformErrorCode::InvalidRequest,
                "governed source refresh source binding is invalid",
            ));
        }
        let mut tx = self.begin_immediate().await?;
        let anchor_row = sqlx::query(
            r#"SELECT
                    registration.source_id,
                    registration.endpoint AS registration_endpoint,
                    anchor.verified_source_id,
                    anchor.endpoint AS anchor_endpoint,
                    anchor.root_digest,
                    anchor.anchor_document_digest,
                    anchor.anchor_document_bytes,
                    anchor.created_at_ms
               FROM governed_source_refresh_registrations registration
               JOIN governed_source_refresh_trust_anchors anchor
                 ON anchor.source_id = registration.source_id
               WHERE registration.source_id = ?"#,
        )
        .bind(&input.source_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(|| {
            error(
                McpPlatformErrorCode::OperationNotSupported,
                "governed source refresh registration has no trusted anchor",
            )
        })?;
        let anchor = decode_governed_source_refresh_trust_anchor_row(&anchor_row)?;
        let pin_row = sqlx::query(
            r#"SELECT source_id, verified_source_id, trust_basis, root_digest, endpoint,
                      source_document_digest, descriptor_digest, created_at_ms
                 FROM governed_source_trust_pins WHERE source_id = ?"#,
        )
        .bind(&input.source_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(|| {
            error(
                McpPlatformErrorCode::OperationNotSupported,
                "governed source refresh requires a trusted user pin",
            )
        })?;
        let pin = decode_governed_source_trust_pin_row(&pin_row)?;
        if pin.trust_basis != GovernedSourceTrustBasis::UserPin
            || pin.source_id != input.source_id
            || pin.verified_source_id != anchor.anchor.source_id
            || pin.root_digest != anchor.anchor.root_digest
            || pin.endpoint != anchor.endpoint
            || pin.source_document_digest != anchor.anchor_document_digest
        {
            return Err(integrity_error());
        }
        if anchor.endpoint != input.expected_endpoint
            || anchor.anchor.root_digest != input.expected_anchor_root_digest
        {
            return Err(error(
                McpPlatformErrorCode::IntegrityError,
                "governed source refresh registration changed during fetch",
            ));
        }
        anchor
            .anchor
            .verify_document(document)
            .map_err(|_| integrity_error())?;
        let SourceRef::VerifiedSourceCatalog {
            source_id: imported_verified_source_id,
        } = &input.import.source.source_ref
        else {
            return Err(error(
                McpPlatformErrorCode::InvalidRequest,
                "governed source refresh requires a verified source catalog",
            ));
        };
        if imported_verified_source_id != &anchor.anchor.source_id {
            return Err(integrity_error());
        }
        let document_outcome =
            crate::verified_source_catalog::dao::store_verified_document_refresh_tx(
                &mut tx,
                document,
                input.import.document.created_at_ms,
            )
            .await
            .map_err(|_| integrity_error())?;
        let outcome = match document_outcome {
            crate::verified_source_catalog::dao::VerifiedDocumentRefreshWriteOutcome::Applied => {
                save_governed_catalog_import_records_in_tx(&mut tx, &input.import, true).await?;
                GovernedSourceRefreshCommitOutcome::Applied
            }
            crate::verified_source_catalog::dao::VerifiedDocumentRefreshWriteOutcome::IdempotentReplay => {
                GovernedSourceRefreshCommitOutcome::IdempotentReplay
            }
        };
        record_governed_source_refresh_audit_in_tx(
            &mut tx,
            &RecordGovernedSourceRefreshAudit {
                source_id: &input.source_id,
                document_digest: Some(document.digests.document_digest.as_str()),
                result: GovernedSourceRefreshResultState::Succeeded,
                error_code: None,
                actor: &input.actor,
                correlation_id: &input.correlation_id,
                occurred_at_ms: input.occurred_at_ms,
            },
        )
        .await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(outcome)
    }

    pub async fn list_governed_source_refresh_registrations(
        &self,
    ) -> McpPlatformResult<Vec<GovernedSourceRefreshRegistrationRecord>> {
        let rows = sqlx::query(
            r#"SELECT
                    source_id, transport_kind, endpoint, created_at_ms, updated_at_ms,
                    last_attempted_at_ms, last_refreshed_at_ms, last_result,
                    last_document_digest, last_error_code
               FROM governed_source_refresh_registrations
               ORDER BY source_id"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        rows.iter()
            .map(decode_governed_source_refresh_registration_row)
            .collect()
    }

    pub async fn record_governed_source_refresh_audit(
        &self,
        input: RecordGovernedSourceRefreshAudit<'_>,
    ) -> McpPlatformResult<GovernedSourceRefreshAuditRecord> {
        let mut tx = self.begin_immediate().await?;
        let audit_id = record_governed_source_refresh_audit_in_tx(&mut tx, &input).await?;
        tx.commit().await.map_err(map_sqlx)?;
        let row = sqlx::query(
            r#"SELECT
                    audit_id, source_id, document_digest, result, error_code,
                    actor, correlation_id, occurred_at_ms
               FROM governed_source_refresh_audits
               WHERE audit_id = ?"#,
        )
        .bind(audit_id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx)?;
        decode_governed_source_refresh_audit_row(&row)
    }

    pub async fn list_governed_source_refresh_audits(
        &self,
        source_id: &str,
    ) -> McpPlatformResult<Vec<GovernedSourceRefreshAuditRecord>> {
        let rows = sqlx::query(
            r#"SELECT
                    audit_id, source_id, document_digest, result, error_code,
                    actor, correlation_id, occurred_at_ms
               FROM governed_source_refresh_audits
               WHERE source_id = ?
               ORDER BY occurred_at_ms DESC, audit_id DESC"#,
        )
        .bind(source_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        rows.iter()
            .map(decode_governed_source_refresh_audit_row)
            .collect()
    }
}

async fn record_governed_source_refresh_audit_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &RecordGovernedSourceRefreshAudit<'_>,
) -> McpPlatformResult<i64> {
    let refreshed_at_ms = if input.result == GovernedSourceRefreshResultState::Succeeded {
        Some(input.occurred_at_ms)
    } else {
        None
    };
    let updated = sqlx::query(
        r#"UPDATE governed_source_refresh_registrations
           SET last_attempted_at_ms = ?,
               last_refreshed_at_ms = COALESCE(?, last_refreshed_at_ms),
               last_result = ?,
               last_document_digest = COALESCE(?, last_document_digest),
               last_error_code = ?
           WHERE source_id = ?"#,
    )
    .bind(input.occurred_at_ms)
    .bind(refreshed_at_ms)
    .bind(input.result.as_str())
    .bind(input.document_digest)
    .bind(input.error_code)
    .bind(input.source_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    if updated.rows_affected() != 1 {
        return Err(not_found());
    }
    sqlx::query_scalar::<_, i64>(
        r#"INSERT INTO governed_source_refresh_audits(
                source_id, document_digest, result, error_code,
                actor, correlation_id, occurred_at_ms
           ) VALUES (?, ?, ?, ?, ?, ?, ?)
           RETURNING audit_id"#,
    )
    .bind(input.source_id)
    .bind(input.document_digest)
    .bind(input.result.as_str())
    .bind(input.error_code)
    .bind(input.actor)
    .bind(input.correlation_id)
    .bind(input.occurred_at_ms)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)
}

fn url_is_in_memory(url: &str) -> bool {
    url.contains(":memory:") || url.contains("mode=memory")
}

fn preflight_new_repository_open(
    signer: &dyn IntegritySigner,
    expected_path_binding: Option<&str>,
) -> McpPlatformResult<()> {
    if let Some(error) = integrity::preflight_open_error(signer) {
        return Err(error);
    }
    let identity = signer.identity()?;
    if integrity::parse_persisted_provider_id(signer.provider_id()).is_none()
        || integrity::parse_persisted_provider_id(&identity.provider_id).is_none()
        || signer.provider_id() != identity.provider_id
    {
        return Err(integrity::integrity_recovery_required());
    }
    if expected_path_binding.is_some_and(|expected| {
        !integrity::matches_expected_path_binding(signer, expected, &identity.path_binding)
    }) {
        return Err(integrity::integrity_recovery_required());
    }
    if signer.checkpoint()?.is_some() {
        return Err(integrity::integrity_recovery_required());
    }
    Ok(())
}

async fn preflight_existing_repository_open(
    path: &Path,
    signer: &dyn IntegritySigner,
    expected_path_binding: Option<&str>,
) -> McpPlatformResult<()> {
    if let Some(error) = integrity::preflight_open_error(signer) {
        return Err(error);
    }
    let pool = SqliteMcpPlatformRepository::readonly_existing_repository_pool(path).await?;
    let result =
        verify_existing_repository_without_writes(&pool, signer, expected_path_binding).await;
    pool.close().await;
    result
}

async fn verify_existing_repository_without_writes(
    pool: &Pool<Sqlite>,
    signer: &dyn IntegritySigner,
    expected_path_binding: Option<&str>,
) -> McpPlatformResult<()> {
    let mut transaction = pool.begin().await.map_err(map_sqlx)?;
    let provider_columns_present =
        integrity_anchor_provider_columns_present(&mut transaction).await?;
    let (_, current_schema_version) = load_schema_version_state(&mut transaction).await?;
    let outcome = match integrity_anchor_schema_state(&mut transaction).await? {
        IntegrityAnchorSchemaState::Anchored => {
            let metadata = load_integrity_metadata(&mut transaction, provider_columns_present)
                .await?
                .ok_or_else(integrity::integrity_recovery_required)?;
            let external = load_or_recover_external_checkpoint(
                &mut transaction,
                signer,
                &metadata,
                expected_path_binding,
                provider_columns_present,
            )
            .await?
            .ok_or_else(integrity::integrity_recovery_required)?;
            let verified = verify_metadata(
                &mut transaction,
                signer,
                &metadata,
                &external,
                expected_path_binding,
                provider_columns_present,
            )
            .await;
            if verified.is_ok() {
                Ok(())
            } else if current_schema_version >= 15
                && matches!(
                    signer.stored_schema_compatibility_fence()?,
                    Some(ref fence) if fence == &expected_v15_schema_fence_pending()
                )
            {
                verify_post_migration_pending_v15_state(
                    &mut transaction,
                    signer,
                    expected_path_binding,
                    &external,
                )
                .await
                .map(|_| ())
            } else {
                verified
            }
        }
        IntegrityAnchorSchemaState::Missing => Err(integrity::integrity_recovery_required()),
    };
    let rollback = transaction.rollback().await.map_err(map_sqlx);
    match (outcome, rollback) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IntegrityAnchorSchemaState {
    Anchored,
    Missing,
}

async fn integrity_anchor_schema_state(
    transaction: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<IntegrityAnchorSchemaState> {
    let tables = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master WHERE type='table' AND name IN ('integrity_metadata','integrity_commits') ORDER BY name",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(map_sqlx)?;
    match tables.as_slice() {
        [] => Ok(IntegrityAnchorSchemaState::Missing),
        [integrity_commits, integrity_metadata]
            if integrity_commits == "integrity_commits"
                && integrity_metadata == "integrity_metadata" =>
        {
            Ok(IntegrityAnchorSchemaState::Anchored)
        }
        _ => Err(integrity::integrity_recovery_required()),
    }
}

async fn integrity_anchor_provider_columns_present(
    transaction: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<bool> {
    let metadata_provider_id = sqlx::query_scalar::<_, String>(
        "SELECT name FROM pragma_table_info('integrity_metadata') WHERE name='provider_id'",
    )
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_sqlx)?
    .is_some();
    let commits_provider_id = sqlx::query_scalar::<_, String>(
        "SELECT name FROM pragma_table_info('integrity_commits') WHERE name='provider_id'",
    )
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_sqlx)?
    .is_some();
    match (metadata_provider_id, commits_provider_id) {
        (true, true) => Ok(true),
        (false, false) => Ok(false),
        _ => Err(integrity::integrity_recovery_required()),
    }
}

#[derive(Debug)]
struct IntegrityMetadata {
    checkpoint: integrity::AnchorCheckpoint,
    state_digest: String,
    commit_mac: String,
    status: String,
    format: IntegrityAnchorFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IntegrityAnchorFormat {
    LegacyV1,
    ProviderBoundV2,
}

#[derive(Debug)]
struct IntegrityCommitRecord {
    sequence: u64,
    instance_id: String,
    key_epoch: u64,
    provider_id: Option<String>,
    parent_root: String,
    state_digest: String,
    root: String,
    commit_mac: String,
}

struct DatabaseLockGuard {
    resolved_path: PathBuf,
    file: Option<File>,
}

impl DatabaseLockGuard {
    fn acquire(path: &Path) -> McpPlatformResult<Self> {
        Self::acquire_with_cleanup(path, !path.exists())
    }

    fn acquire_persistent(path: &Path) -> McpPlatformResult<Self> {
        Self::acquire_with_cleanup(path, false)
    }

    fn acquire_with_cleanup(path: &Path, _cleanup_on_error: bool) -> McpPlatformResult<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|_| repository_unavailable())?;
        file.lock_exclusive()
            .map_err(|_| integrity::integrity_recovery_required())?;
        let resolved_path = path.canonicalize().map_err(|_| repository_unavailable())?;
        #[cfg(test)]
        run_after_database_lock_acquired_hook(path, &resolved_path);
        Ok(Self {
            resolved_path,
            file: Some(file),
        })
    }

    fn resolved_path(&self) -> &Path {
        &self.resolved_path
    }

    fn disarm_cleanup(&mut self) {}

    fn cleanup(mut self) -> McpPlatformResult<()> {
        self.file.take();
        Ok(())
    }
}

async fn initialize_or_verify_integrity(
    pool: &Pool<Sqlite>,
    signer: &dyn IntegritySigner,
    allow_bootstrap: bool,
    expected_path_binding: Option<&str>,
) -> McpPlatformResult<()> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(map_sqlx)?;
    let metadata = load_integrity_metadata(&mut transaction, true).await?;
    let external = match metadata.as_ref() {
        Some(metadata) => {
            load_or_recover_external_checkpoint(
                &mut transaction,
                signer,
                metadata,
                expected_path_binding,
                true,
            )
            .await?
        }
        None => signer.checkpoint()?,
    };
    match (metadata, external) {
        (Some(metadata), Some(external)) => {
            verify_metadata(
                &mut transaction,
                signer,
                &metadata,
                &external,
                expected_path_binding,
                true,
            )
            .await?;
            match metadata.format {
                IntegrityAnchorFormat::LegacyV1 => {
                    migrate_legacy_integrity_provider_binding(
                        &mut transaction,
                        signer,
                        &metadata,
                        &external,
                        expected_path_binding,
                    )
                    .await?;
                }
                IntegrityAnchorFormat::ProviderBoundV2 => {
                    maybe_migrate_external_provider_binding(signer, &external)?;
                }
            }
            transaction.commit().await.map_err(map_sqlx)
        }
        (None, None) if allow_bootstrap => {
            let identity = signer.identity()?;
            if let Some(expected) = expected_path_binding {
                if identity.path_binding != expected
                    && !integrity::matches_expected_path_binding(
                        signer,
                        expected,
                        &identity.path_binding,
                    )
                {
                    return Err(integrity::integrity_recovery_required());
                }
            }
            let state_digest = security_state_digest(&mut transaction).await?;
            let checkpoint = genesis_checkpoint(identity, &state_digest);
            let commit_mac = sign_checkpoint(signer, &checkpoint, "", &state_digest)?;
            sqlx::query(
                "INSERT INTO integrity_commits(sequence,instance_id,key_epoch,provider_id,parent_root,state_digest,root,commit_mac) VALUES (0,?,?,?,?,?,?,?)",
            )
            .bind(&checkpoint.identity.instance_id)
            .bind(i64::try_from(checkpoint.identity.key_epoch).map_err(|_| integrity::integrity_recovery_required())?)
            .bind(&checkpoint.identity.provider_id)
            .bind("")
            .bind(&state_digest)
            .bind(&checkpoint.root)
            .bind(&commit_mac)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
            sqlx::query(
                "INSERT INTO integrity_metadata(singleton,instance_id,path_binding,key_epoch,provider_id,sequence,root,state_digest,commit_mac,status) VALUES (1,?,?,?,?,?,?,?,?,'active')",
            )
            .bind(&checkpoint.identity.instance_id)
            .bind(&checkpoint.identity.path_binding)
            .bind(i64::try_from(checkpoint.identity.key_epoch).map_err(|_| integrity::integrity_recovery_required())?)
            .bind(&checkpoint.identity.provider_id)
            .bind(0_i64)
            .bind(&checkpoint.root)
            .bind(&state_digest)
            .bind(&commit_mac)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx)?;
            transaction.commit().await.map_err(map_sqlx)?;
            signer.publish(None, &checkpoint)
        }
        _ => Err(integrity::integrity_recovery_required()),
    }
}

fn maybe_migrate_external_provider_binding(
    signer: &dyn IntegritySigner,
    external: &integrity::AnchorCheckpoint,
) -> McpPlatformResult<()> {
    if signer.needs_persisted_provider_migration()? {
        signer.migrate_persisted_provider_binding(external)?;
        if signer.needs_persisted_provider_migration()? {
            return Err(integrity::integrity_recovery_required());
        }
    }
    Ok(())
}

async fn migrate_legacy_integrity_provider_binding(
    transaction: &mut Transaction<'_, Sqlite>,
    signer: &dyn IntegritySigner,
    metadata: &IntegrityMetadata,
    external: &integrity::AnchorCheckpoint,
    expected_path_binding: Option<&str>,
) -> McpPlatformResult<()> {
    let current_provider_id = verify_external_provider_binding(signer, external)?;
    maybe_migrate_external_provider_binding(signer, external)?;
    let rows = sqlx::query(
        "SELECT sequence,instance_id,key_epoch,provider_id,parent_root,state_digest,root,commit_mac FROM integrity_commits ORDER BY sequence",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(map_sqlx)?;
    let mut metadata_parent_root = None;
    for row in rows {
        let commit = decode_integrity_commit_row(&row, true)?;
        if commit.provider_id.is_some() {
            return Err(integrity::integrity_recovery_required());
        }
        let checkpoint = integrity::AnchorCheckpoint {
            identity: external.identity.clone(),
            sequence: commit.sequence,
            root: commit.root.clone(),
        };
        let provider_bound_mac = sign_checkpoint_with_format(
            signer,
            IntegrityAnchorFormat::ProviderBoundV2,
            &checkpoint,
            &commit.parent_root,
            &commit.state_digest,
        )?;
        let updated = sqlx::query(
            "UPDATE integrity_commits SET provider_id=?, commit_mac=? WHERE sequence=? AND instance_id=? AND key_epoch=? AND provider_id IS NULL AND parent_root=? AND state_digest=? AND root=? AND commit_mac=?",
        )
        .bind(current_provider_id.as_str())
        .bind(&provider_bound_mac)
        .bind(i64::try_from(commit.sequence).map_err(|_| integrity::integrity_recovery_required())?)
        .bind(&commit.instance_id)
        .bind(i64::try_from(commit.key_epoch).map_err(|_| integrity::integrity_recovery_required())?)
        .bind(&commit.parent_root)
        .bind(&commit.state_digest)
        .bind(&commit.root)
        .bind(&commit.commit_mac)
        .execute(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        if updated.rows_affected() != 1 {
            return Err(integrity::integrity_recovery_required());
        }
        if commit.sequence == external.sequence {
            metadata_parent_root = Some(commit.parent_root);
        }
    }
    let metadata_parent_root =
        metadata_parent_root.ok_or_else(integrity::integrity_recovery_required)?;
    let provider_bound_metadata_mac = sign_checkpoint_with_format(
        signer,
        IntegrityAnchorFormat::ProviderBoundV2,
        external,
        &metadata_parent_root,
        &metadata.state_digest,
    )?;
    let updated = sqlx::query(
        "UPDATE integrity_metadata SET provider_id=?, commit_mac=? WHERE singleton=1 AND instance_id=? AND path_binding=? AND key_epoch=? AND provider_id IS NULL AND sequence=? AND root=? AND state_digest=? AND commit_mac=? AND status='active'",
    )
    .bind(current_provider_id.as_str())
    .bind(&provider_bound_metadata_mac)
    .bind(&external.identity.instance_id)
    .bind(&external.identity.path_binding)
    .bind(i64::try_from(external.identity.key_epoch).map_err(|_| integrity::integrity_recovery_required())?)
    .bind(i64::try_from(external.sequence).map_err(|_| integrity::integrity_recovery_required())?)
    .bind(&external.root)
    .bind(&metadata.state_digest)
    .bind(&metadata.commit_mac)
    .execute(&mut **transaction)
    .await
    .map_err(map_sqlx)?;
    if updated.rows_affected() != 1 {
        return Err(integrity::integrity_recovery_required());
    }
    run_after_legacy_provider_binding_database_rewrite_hook()?;
    let migrated = load_integrity_metadata(transaction, true)
        .await?
        .ok_or_else(integrity::integrity_recovery_required)?;
    if migrated.format != IntegrityAnchorFormat::ProviderBoundV2 {
        return Err(integrity::integrity_recovery_required());
    }
    verify_metadata(
        transaction,
        signer,
        &migrated,
        external,
        expected_path_binding,
        true,
    )
    .await
}

async fn verify_integrity_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    signer: &dyn IntegritySigner,
) -> McpPlatformResult<integrity::AnchorCheckpoint> {
    let metadata = load_integrity_metadata(transaction, true)
        .await?
        .ok_or_else(integrity::integrity_recovery_required)?;
    let external = load_or_recover_external_checkpoint(transaction, signer, &metadata, None, true)
        .await?
        .ok_or_else(integrity::integrity_recovery_required)?;
    verify_metadata(transaction, signer, &metadata, &external, None, true).await?;
    Ok(external)
}

pub(super) async fn recovery_eligibility_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<RecoveryEligibility> {
    let blocked: McpPlatformResult<bool> = async {
        let schema_version = sqlx::query_scalar::<_, i64>(
            "SELECT MAX(version) FROM schema_version",
        )
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
        if schema_version < migrations::CURRENT_SCHEMA_VERSION {
            return Ok(true);
        }
        if schema_version >= 39 {
            migrations::validate_effect_fence_state(transaction).await?;
        }
        if schema_version >= 43 {
            migrations::validate_fenced_effect_authority_v43_state(transaction).await?;
        } else if schema_version >= 42 {
            migrations::validate_fenced_effect_receipt_state(transaction).await?;
        }
        let disposition_mismatch = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM mcp_profile_lifecycle_impact_v36_migration_dispositions d LEFT JOIN mcp_profile_lifecycle_impact_v38_disposition_canonicals c ON c.disposition_digest=d.disposition_digest WHERE c.disposition_digest IS NULL OR c.migration_domain IS NOT d.migration_domain OR c.source_table IS NOT d.source_table OR c.source_rowid IS NOT d.source_rowid OR c.legacy_row_type IS NOT d.legacy_row_type OR c.status IS NOT d.status OR c.migrated_at_ms IS NOT d.migrated_at_ms UNION ALL SELECT COUNT(*) FROM mcp_profile_lifecycle_impact_v38_disposition_canonicals c LEFT JOIN mcp_profile_lifecycle_impact_v36_migration_dispositions d ON d.disposition_digest=c.disposition_digest WHERE d.disposition_digest IS NULL OR d.migration_domain IS NOT c.migration_domain OR d.source_table IS NOT c.source_table OR d.source_rowid IS NOT c.source_rowid OR d.legacy_row_type IS NOT c.legacy_row_type OR d.status IS NOT c.status OR d.migrated_at_ms IS NOT c.migrated_at_ms",
        )
        .fetch_all(&mut **transaction)
        .await
        .map_err(map_sqlx)?
        .into_iter()
        .any(|count| count != 0);
        if disposition_mismatch {
            return Ok(true);
        }
        let observed_record_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mcp_profile_lifecycle_impact_v35_migration_audits WHERE status='discarded_integrity'")
            .fetch_one(&mut **transaction).await.map_err(map_sqlx)?;
        let attestation_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mcp_profile_lifecycle_audit_integrity_attestations")
            .fetch_one(&mut **transaction).await.map_err(map_sqlx)?;
        let canonical_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mcp_profile_lifecycle_audit_integrity_attestation_canonicals")
            .fetch_one(&mut **transaction).await.map_err(map_sqlx)?;
        if attestation_count != canonical_count || attestation_count > 1 || (observed_record_count > 0) != (attestation_count == 1) {
            return Ok(true);
        }
        if attestation_count == 1 {
            let attestation_mismatch = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mcp_profile_lifecycle_audit_integrity_attestations a LEFT JOIN mcp_profile_lifecycle_audit_integrity_attestation_canonicals c ON c.attestation_id=a.attestation_id WHERE c.attestation_id IS NULL OR c.migration_domain IS NOT a.migration_domain OR c.version_from IS NOT a.version_from OR c.version_through IS NOT a.version_through OR c.status IS NOT a.status OR c.observed_record_count IS NOT a.observed_record_count OR c.created_at_ms IS NOT a.created_at_ms")
                .fetch_one(&mut **transaction).await.map_err(map_sqlx)?;
            let count_matches = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mcp_profile_lifecycle_audit_integrity_attestations WHERE observed_record_count=?")
                .bind(observed_record_count).fetch_one(&mut **transaction).await.map_err(map_sqlx)?;
            if attestation_mismatch != 0 || count_matches != 1 {
                return Ok(true);
            }
            return Ok(true);
        }
        Ok(false)
    }
    .await;
    let blocked =
        blocked.map_err(|_| error(McpPlatformErrorCode::IntegrityError, "recovery_blocked"))?;
    Ok(if blocked {
        RecoveryEligibility::Blocked
    } else {
        RecoveryEligibility::Eligible
    })
}

async fn verified_external_checkpoint(
    pool: &Pool<Sqlite>,
    signer: &dyn IntegritySigner,
    expected_path_binding: Option<&str>,
) -> McpPlatformResult<integrity::AnchorCheckpoint> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(map_sqlx)?;
    let metadata = load_integrity_metadata(&mut transaction, true)
        .await?
        .ok_or_else(integrity::integrity_recovery_required)?;
    let external = load_or_recover_external_checkpoint(
        &mut transaction,
        signer,
        &metadata,
        expected_path_binding,
        true,
    )
    .await?
    .ok_or_else(integrity::integrity_recovery_required)?;
    verify_metadata(
        &mut transaction,
        signer,
        &metadata,
        &external,
        expected_path_binding,
        true,
    )
    .await?;
    transaction.rollback().await.map_err(map_sqlx)?;
    Ok(external)
}

async fn verify_post_migration_pending_v15_state(
    transaction: &mut Transaction<'_, Sqlite>,
    signer: &dyn IntegritySigner,
    expected_path_binding: Option<&str>,
    external: &integrity::AnchorCheckpoint,
) -> McpPlatformResult<integrity::AnchorCheckpoint> {
    migrations::validate_projection_mutations_schema(transaction).await?;
    let metadata = load_integrity_metadata(transaction, true)
        .await?
        .ok_or_else(integrity::integrity_recovery_required)?;
    verify_metadata(
        transaction,
        signer,
        &metadata,
        &metadata.checkpoint,
        expected_path_binding,
        true,
    )
    .await?;
    if metadata.format != IntegrityAnchorFormat::ProviderBoundV2
        || metadata.checkpoint.identity != external.identity
        || metadata.checkpoint.sequence
            != external
                .sequence
                .checked_add(1)
                .ok_or_else(integrity::integrity_recovery_required)?
    {
        return Err(integrity::integrity_recovery_required());
    }
    let current_provider_id = verify_external_provider_binding(signer, external)?;
    let parent_root = load_commit_parent_root(
        transaction,
        metadata.format,
        current_provider_id,
        &metadata.checkpoint,
        &metadata.state_digest,
        &metadata.commit_mac,
        true,
    )
    .await?;
    if parent_root != external.root {
        return Err(integrity::integrity_recovery_required());
    }
    Ok(metadata.checkpoint)
}

async fn verify_post_migration_pending_v15_checkpoint(
    pool: &Pool<Sqlite>,
    signer: &dyn IntegritySigner,
    expected_path_binding: Option<&str>,
    external: &integrity::AnchorCheckpoint,
) -> McpPlatformResult<integrity::AnchorCheckpoint> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(map_sqlx)?;
    let checkpoint = verify_post_migration_pending_v15_state(
        &mut transaction,
        signer,
        expected_path_binding,
        external,
    )
    .await?;
    transaction.rollback().await.map_err(map_sqlx)?;
    Ok(checkpoint)
}

async fn load_or_recover_external_checkpoint(
    transaction: &mut Transaction<'_, Sqlite>,
    signer: &dyn IntegritySigner,
    metadata: &IntegrityMetadata,
    expected_path_binding: Option<&str>,
    provider_columns_present: bool,
) -> McpPlatformResult<Option<integrity::AnchorCheckpoint>> {
    let external = signer.checkpoint()?;
    if external.is_some()
        || !matches!(
            signer.assurance(),
            integrity::AnchorProviderAssurance::TestInMemory
        )
    {
        return Ok(external);
    }
    if signer.identity()? != metadata.checkpoint.identity {
        return Ok(None);
    }
    verify_metadata(
        transaction,
        signer,
        metadata,
        &metadata.checkpoint,
        expected_path_binding,
        provider_columns_present,
    )
    .await?;
    replay_external_checkpoint_chain(
        transaction,
        signer,
        &metadata.checkpoint.identity,
        provider_columns_present,
    )
    .await?;
    signer.checkpoint()
}

async fn replay_external_checkpoint_chain(
    transaction: &mut Transaction<'_, Sqlite>,
    signer: &dyn IntegritySigner,
    identity: &integrity::AnchorIdentity,
    provider_columns_present: bool,
) -> McpPlatformResult<()> {
    let rows = if provider_columns_present {
        sqlx::query(
            "SELECT sequence,instance_id,key_epoch,provider_id,parent_root,state_digest,root,commit_mac FROM integrity_commits ORDER BY sequence",
        )
        .fetch_all(&mut **transaction)
        .await
        .map_err(map_sqlx)?
    } else {
        sqlx::query(
            "SELECT sequence,instance_id,key_epoch,parent_root,state_digest,root,commit_mac FROM integrity_commits ORDER BY sequence",
        )
        .fetch_all(&mut **transaction)
        .await
        .map_err(map_sqlx)?
    };
    let mut expected = None;
    for row in rows {
        let commit = decode_integrity_commit_row(&row, provider_columns_present)?;
        let checkpoint = integrity::AnchorCheckpoint {
            identity: identity.clone(),
            sequence: commit.sequence,
            root: commit.root,
        };
        signer.publish(expected.as_ref(), &checkpoint)?;
        expected = Some(checkpoint);
    }
    Ok(())
}

async fn verify_metadata(
    transaction: &mut Transaction<'_, Sqlite>,
    signer: &dyn IntegritySigner,
    metadata: &IntegrityMetadata,
    external: &integrity::AnchorCheckpoint,
    expected_path_binding: Option<&str>,
    provider_columns_present: bool,
) -> McpPlatformResult<()> {
    let current_provider_id = verify_external_provider_binding(signer, external)?;
    if metadata.status != "active"
        || !checkpoint_matches_format(metadata.format, &metadata.checkpoint, external)
        || expected_path_binding.is_some_and(|expected| {
            !integrity::matches_expected_path_binding(
                signer,
                expected,
                &external.identity.path_binding,
            )
        })
        || matches!(
            metadata.format,
            IntegrityAnchorFormat::ProviderBoundV2
                if metadata.checkpoint.identity.provider_id != current_provider_id.as_str()
        )
    {
        return Err(integrity::integrity_recovery_required());
    }
    let state_digest = security_state_digest(transaction).await?;
    if state_digest != metadata.state_digest {
        return Err(integrity::integrity_recovery_required());
    }
    verify_commit_chain(
        transaction,
        signer,
        external,
        metadata.format,
        provider_columns_present,
    )
    .await?;
    let parent_root = load_commit_parent_root(
        transaction,
        metadata.format,
        current_provider_id,
        external,
        &metadata.state_digest,
        &metadata.commit_mac,
        provider_columns_present,
    )
    .await?;
    let expected_root = checkpoint_root(
        &external.identity,
        external.sequence,
        &parent_root,
        &metadata.state_digest,
    );
    if expected_root != external.root {
        return Err(integrity::integrity_recovery_required());
    }
    signer.verify(
        checkpoint_domain(metadata.format),
        &checkpoint_payload(
            metadata.format,
            external,
            &parent_root,
            &metadata.state_digest,
        ),
        &metadata.commit_mac,
    )
}

async fn verify_commit_chain(
    transaction: &mut Transaction<'_, Sqlite>,
    signer: &dyn IntegritySigner,
    external: &integrity::AnchorCheckpoint,
    format: IntegrityAnchorFormat,
    provider_columns_present: bool,
) -> McpPlatformResult<()> {
    let current_provider_id = verify_external_provider_binding(signer, external)?;
    let rows = if provider_columns_present {
        sqlx::query(
            "SELECT sequence,instance_id,key_epoch,provider_id,parent_root,state_digest,root,commit_mac FROM integrity_commits ORDER BY sequence",
        )
        .fetch_all(&mut **transaction)
        .await
        .map_err(map_sqlx)?
    } else {
        sqlx::query(
            "SELECT sequence,instance_id,key_epoch,parent_root,state_digest,root,commit_mac FROM integrity_commits ORDER BY sequence",
        )
        .fetch_all(&mut **transaction)
        .await
        .map_err(map_sqlx)?
    };
    let expected_len = external
        .sequence
        .checked_add(1)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(integrity::integrity_recovery_required)?;
    if rows.len() != expected_len {
        return Err(integrity::integrity_recovery_required());
    }
    let mut parent_root = String::new();
    for (expected_sequence, row) in rows.into_iter().enumerate() {
        let commit = decode_integrity_commit_row(&row, provider_columns_present)?;
        let checkpoint = integrity::AnchorCheckpoint {
            identity: external.identity.clone(),
            sequence: commit.sequence,
            root: commit.root.clone(),
        };
        if commit.sequence != expected_sequence as u64
            || commit.instance_id != external.identity.instance_id
            || commit.key_epoch != external.identity.key_epoch
            || commit.parent_root != parent_root
            || !commit_provider_matches_format(
                format,
                current_provider_id,
                commit.provider_id.as_deref(),
            )
            || checkpoint_root(
                &external.identity,
                commit.sequence,
                &commit.parent_root,
                &commit.state_digest,
            ) != commit.root
        {
            return Err(integrity::integrity_recovery_required());
        }
        signer.verify(
            checkpoint_domain(format),
            &checkpoint_payload(
                format,
                &checkpoint,
                &commit.parent_root,
                &commit.state_digest,
            ),
            &commit.commit_mac,
        )?;
        parent_root = commit.root;
    }
    if parent_root != external.root {
        return Err(integrity::integrity_recovery_required());
    }
    Ok(())
}

async fn load_integrity_metadata(
    transaction: &mut Transaction<'_, Sqlite>,
    provider_columns_present: bool,
) -> McpPlatformResult<Option<IntegrityMetadata>> {
    let row = if provider_columns_present {
        sqlx::query(
            "SELECT instance_id,path_binding,key_epoch,provider_id,sequence,root,state_digest,commit_mac,status FROM integrity_metadata WHERE singleton=1",
        )
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx)?
    } else {
        sqlx::query(
            "SELECT instance_id,path_binding,key_epoch,sequence,root,state_digest,commit_mac,status FROM integrity_metadata WHERE singleton=1",
        )
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx)?
    };
    row.map(|row| {
        let provider_id: Option<String> = if provider_columns_present {
            row.try_get("provider_id").map_err(map_sqlx)?
        } else {
            None
        };
        let key_epoch = u64::try_from(row.try_get::<i64, _>("key_epoch").map_err(map_sqlx)?)
            .map_err(|_| integrity::integrity_recovery_required())?;
        let sequence = u64::try_from(row.try_get::<i64, _>("sequence").map_err(map_sqlx)?)
            .map_err(|_| integrity::integrity_recovery_required())?;
        let (format, provider_id) = match provider_id {
            Some(provider_id) => (
                IntegrityAnchorFormat::ProviderBoundV2,
                integrity::parse_persisted_provider_id(&provider_id)
                    .ok_or_else(integrity::integrity_recovery_required)?
                    .as_str()
                    .to_string(),
            ),
            None => (IntegrityAnchorFormat::LegacyV1, String::new()),
        };
        Ok(IntegrityMetadata {
            checkpoint: integrity::AnchorCheckpoint {
                identity: integrity::AnchorIdentity {
                    instance_id: row.try_get("instance_id").map_err(map_sqlx)?,
                    path_binding: row.try_get("path_binding").map_err(map_sqlx)?,
                    key_epoch,
                    provider_id,
                },
                sequence,
                root: row.try_get("root").map_err(map_sqlx)?,
            },
            state_digest: row.try_get("state_digest").map_err(map_sqlx)?,
            commit_mac: row.try_get("commit_mac").map_err(map_sqlx)?,
            status: row.try_get("status").map_err(map_sqlx)?,
            format,
        })
    })
    .transpose()
}

fn genesis_checkpoint(
    identity: integrity::AnchorIdentity,
    state_digest: &str,
) -> integrity::AnchorCheckpoint {
    let root = checkpoint_root(&identity, 0, "", state_digest);
    integrity::AnchorCheckpoint {
        identity,
        sequence: 0,
        root,
    }
}

fn next_checkpoint(
    current: &integrity::AnchorCheckpoint,
    state_digest: &str,
) -> integrity::AnchorCheckpoint {
    let sequence = current.sequence + 1;
    integrity::AnchorCheckpoint {
        identity: current.identity.clone(),
        sequence,
        root: checkpoint_root(&current.identity, sequence, &current.root, state_digest),
    }
}

fn checkpoint_root(
    identity: &integrity::AnchorIdentity,
    sequence: u64,
    parent_root: &str,
    state_digest: &str,
) -> String {
    integrity::digest(&[
        b"lumina.mcp-platform.commit-root",
        b"v1",
        identity.instance_id.as_bytes(),
        identity.path_binding.as_bytes(),
        identity.key_epoch.to_string().as_bytes(),
        sequence.to_string().as_bytes(),
        parent_root.as_bytes(),
        state_digest.as_bytes(),
    ])
}

fn checkpoint_payload(
    format: IntegrityAnchorFormat,
    checkpoint: &integrity::AnchorCheckpoint,
    parent_root: &str,
    state_digest: &str,
) -> Vec<u8> {
    match format {
        IntegrityAnchorFormat::LegacyV1 => integrity::canonical_fields(&[
            b"lumina.mcp-platform.anchored-commit",
            b"v1",
            checkpoint.identity.instance_id.as_bytes(),
            checkpoint.identity.path_binding.as_bytes(),
            checkpoint.identity.key_epoch.to_string().as_bytes(),
            checkpoint.sequence.to_string().as_bytes(),
            parent_root.as_bytes(),
            state_digest.as_bytes(),
            checkpoint.root.as_bytes(),
        ]),
        IntegrityAnchorFormat::ProviderBoundV2 => integrity::canonical_fields(&[
            b"lumina.mcp-platform.anchored-commit",
            b"v2",
            checkpoint.identity.provider_id.as_bytes(),
            checkpoint.identity.instance_id.as_bytes(),
            checkpoint.identity.path_binding.as_bytes(),
            checkpoint.identity.key_epoch.to_string().as_bytes(),
            checkpoint.sequence.to_string().as_bytes(),
            parent_root.as_bytes(),
            state_digest.as_bytes(),
            checkpoint.root.as_bytes(),
        ]),
    }
}

fn projection_authority_anchor(
    checkpoint: &integrity::AnchorCheckpoint,
) -> ProjectionAuthorityAnchor {
    ProjectionAuthorityAnchor {
        instance_id: checkpoint.identity.instance_id.clone(),
        path_binding: checkpoint.identity.path_binding.clone(),
        key_epoch: checkpoint.identity.key_epoch,
        sequence: checkpoint.sequence,
        root: checkpoint.root.clone(),
    }
}

fn sign_checkpoint(
    signer: &dyn IntegritySigner,
    checkpoint: &integrity::AnchorCheckpoint,
    parent_root: &str,
    state_digest: &str,
) -> McpPlatformResult<String> {
    sign_checkpoint_with_format(
        signer,
        IntegrityAnchorFormat::ProviderBoundV2,
        checkpoint,
        parent_root,
        state_digest,
    )
}

fn sign_checkpoint_with_format(
    signer: &dyn IntegritySigner,
    format: IntegrityAnchorFormat,
    checkpoint: &integrity::AnchorCheckpoint,
    parent_root: &str,
    state_digest: &str,
) -> McpPlatformResult<String> {
    signer.sign(
        checkpoint_domain(format),
        &checkpoint_payload(format, checkpoint, parent_root, state_digest),
    )
}

fn checkpoint_domain(format: IntegrityAnchorFormat) -> &'static str {
    match format {
        IntegrityAnchorFormat::LegacyV1 => "anchored-commit-v1",
        IntegrityAnchorFormat::ProviderBoundV2 => "anchored-commit-v2",
    }
}

fn verify_external_provider_binding(
    signer: &dyn IntegritySigner,
    external: &integrity::AnchorCheckpoint,
) -> McpPlatformResult<integrity::PersistedProviderId> {
    let signer_provider_id = integrity::parse_persisted_provider_id(signer.provider_id())
        .ok_or_else(integrity::integrity_recovery_required)?;
    let checkpoint_provider_id =
        integrity::parse_persisted_provider_id(&external.identity.provider_id)
            .ok_or_else(integrity::integrity_recovery_required)?;
    if signer_provider_id != checkpoint_provider_id {
        return Err(integrity::integrity_recovery_required());
    }
    Ok(signer_provider_id)
}

fn checkpoint_matches_format(
    format: IntegrityAnchorFormat,
    stored: &integrity::AnchorCheckpoint,
    external: &integrity::AnchorCheckpoint,
) -> bool {
    match format {
        IntegrityAnchorFormat::LegacyV1 => {
            stored.identity.instance_id == external.identity.instance_id
                && stored.identity.path_binding == external.identity.path_binding
                && stored.identity.key_epoch == external.identity.key_epoch
                && stored.sequence == external.sequence
                && stored.root == external.root
        }
        IntegrityAnchorFormat::ProviderBoundV2 => stored == external,
    }
}

fn commit_provider_matches_format(
    format: IntegrityAnchorFormat,
    current_provider_id: integrity::PersistedProviderId,
    provider_id: Option<&str>,
) -> bool {
    match format {
        IntegrityAnchorFormat::LegacyV1 => provider_id.is_none(),
        IntegrityAnchorFormat::ProviderBoundV2 => provider_id == Some(current_provider_id.as_str()),
    }
}

fn decode_integrity_commit_row(
    row: &sqlx::sqlite::SqliteRow,
    provider_columns_present: bool,
) -> McpPlatformResult<IntegrityCommitRecord> {
    Ok(IntegrityCommitRecord {
        sequence: u64::try_from(row.try_get::<i64, _>("sequence").map_err(map_sqlx)?)
            .map_err(|_| integrity::integrity_recovery_required())?,
        instance_id: row.try_get("instance_id").map_err(map_sqlx)?,
        key_epoch: u64::try_from(row.try_get::<i64, _>("key_epoch").map_err(map_sqlx)?)
            .map_err(|_| integrity::integrity_recovery_required())?,
        provider_id: if provider_columns_present {
            row.try_get("provider_id").map_err(map_sqlx)?
        } else {
            None
        },
        parent_root: row.try_get("parent_root").map_err(map_sqlx)?,
        state_digest: row.try_get("state_digest").map_err(map_sqlx)?,
        root: row.try_get("root").map_err(map_sqlx)?,
        commit_mac: row.try_get("commit_mac").map_err(map_sqlx)?,
    })
}

async fn load_commit_parent_root(
    transaction: &mut Transaction<'_, Sqlite>,
    format: IntegrityAnchorFormat,
    current_provider_id: integrity::PersistedProviderId,
    external: &integrity::AnchorCheckpoint,
    state_digest: &str,
    commit_mac: &str,
    provider_columns_present: bool,
) -> McpPlatformResult<String> {
    let sequence =
        i64::try_from(external.sequence).map_err(|_| integrity::integrity_recovery_required())?;
    let parent_root = match (format, provider_columns_present) {
        (IntegrityAnchorFormat::LegacyV1, false) => {
            sqlx::query_scalar::<_, String>(
                "SELECT parent_root FROM integrity_commits WHERE sequence=? AND root=? AND state_digest=? AND commit_mac=?",
            )
            .bind(sequence)
            .bind(&external.root)
            .bind(state_digest)
            .bind(commit_mac)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(map_sqlx)?
        }
        (IntegrityAnchorFormat::LegacyV1, true) => {
            sqlx::query_scalar::<_, String>(
                "SELECT parent_root FROM integrity_commits WHERE sequence=? AND provider_id IS NULL AND root=? AND state_digest=? AND commit_mac=?",
            )
            .bind(sequence)
            .bind(&external.root)
            .bind(state_digest)
            .bind(commit_mac)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(map_sqlx)?
        }
        (IntegrityAnchorFormat::ProviderBoundV2, true) => {
            sqlx::query_scalar::<_, String>(
                "SELECT parent_root FROM integrity_commits WHERE sequence=? AND provider_id=? AND root=? AND state_digest=? AND commit_mac=?",
            )
            .bind(sequence)
            .bind(current_provider_id.as_str())
            .bind(&external.root)
            .bind(state_digest)
            .bind(commit_mac)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(map_sqlx)?
        }
        (IntegrityAnchorFormat::ProviderBoundV2, false) => {
            return Err(integrity::integrity_recovery_required());
        }
    };
    parent_root.ok_or_else(integrity::integrity_recovery_required)
}

async fn security_state_digest(
    transaction: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<String> {
    let schema = sqlx::query(
        "SELECT type,name,COALESCE(sql,'') AS sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name,sql",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(map_sqlx)?;
    let mut hasher = Sha256::new();
    hash_component(&mut hasher, b"lumina.mcp-platform.security-state-v1");
    for row in schema {
        let kind: String = row.try_get("type").map_err(map_sqlx)?;
        let name: String = row.try_get("name").map_err(map_sqlx)?;
        let sql: String = row.try_get("sql").map_err(map_sqlx)?;
        hash_component(&mut hasher, kind.as_bytes());
        hash_component(&mut hasher, name.as_bytes());
        hash_component(&mut hasher, sql.as_bytes());
    }
    let tables = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT IN ('integrity_metadata','integrity_commits') ORDER BY name",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(map_sqlx)?;
    for table in tables {
        hash_component(&mut hasher, table.as_bytes());
        let pragma = format!("PRAGMA table_info({})", quoted_identifier(&table));
        let columns = sqlx::query(&pragma)
            .fetch_all(&mut **transaction)
            .await
            .map_err(map_sqlx)?
            .into_iter()
            .map(|row| row.try_get::<String, _>("name").map_err(map_sqlx))
            .collect::<McpPlatformResult<Vec<_>>>()?;
        let values = columns
            .iter()
            .map(|column| canonical_sql_value(&quoted_identifier(column)))
            .collect::<Vec<_>>();
        let combined = if values.is_empty() {
            "''".to_string()
        } else {
            values.join(" || '|' || ")
        };
        let query = format!(
            "SELECT {combined} AS canonical_row FROM {} ORDER BY canonical_row",
            quoted_identifier(&table)
        );
        let rows = sqlx::query_scalar::<_, String>(&query)
            .fetch_all(&mut **transaction)
            .await
            .map_err(map_sqlx)?;
        for row in rows {
            hash_component(&mut hasher, row.as_bytes());
        }
    }
    Ok(crate::utils::bytes_to_hex(hasher.finalize()))
}

fn canonical_sql_value(identifier: &str) -> String {
    format!(
        "typeof({identifier}) || ':' || length(CASE WHEN typeof({identifier})='blob' THEN hex({identifier}) ELSE quote({identifier}) END) || ':' || CASE WHEN typeof({identifier})='blob' THEN hex({identifier}) ELSE quote({identifier}) END"
    )
}

fn quoted_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn hash_component(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn integrity_lock_path(path: &Path) -> PathBuf {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".integrity.lock");
    PathBuf::from(lock_path)
}

fn locked_database_target(
    path: &Path,
    lock_path: &Path,
    expected_path_binding: &str,
) -> McpPlatformResult<LockedDatabaseTarget> {
    let database_path = resolve_database_path_under_lock(path, lock_path)?;
    let path_binding = database_path_binding_from_resolved_path(&database_path)?;
    if path_binding != expected_path_binding {
        return Err(integrity::integrity_recovery_required());
    }
    let lock_path = integrity_lock_path(&database_path);
    let database_parent = database_path.parent().ok_or_else(repository_unavailable)?;
    let lock_parent = lock_path.parent().ok_or_else(repository_unavailable)?;
    if database_parent != lock_parent {
        return Err(integrity::integrity_recovery_required());
    }
    Ok(LockedDatabaseTarget {
        existed_before_open: database_path.exists(),
        database_path,
        lock_path,
        path_binding,
    })
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

const SQLITE_DATABASE_AND_SIDECAR_SUFFIXES: [&str; 4] = ["", "-journal", "-wal", "-shm"];

fn sqlite_database_and_sidecar_paths(path: &Path) -> Vec<PathBuf> {
    SQLITE_DATABASE_AND_SIDECAR_SUFFIXES
        .iter()
        .map(|suffix| {
            if suffix.is_empty() {
                path.to_path_buf()
            } else {
                path_with_suffix(path, suffix)
            }
        })
        .collect()
}

fn trusted_reenrollment_quarantine_path(path: &Path) -> PathBuf {
    path_with_suffix(
        path,
        &format!(
            ".quarantine-{}",
            trusted_reenrollment_quarantine_token(path)
        ),
    )
}

#[cfg(not(test))]
fn trusted_reenrollment_quarantine_token(_path: &Path) -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
fn trusted_reenrollment_quarantine_token(path: &Path) -> String {
    trusted_reenrollment_quarantine_tokens()
        .lock()
        .unwrap()
        .pop_front(path)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

fn resolve_database_path_under_lock(path: &Path, lock_path: &Path) -> McpPlatformResult<PathBuf> {
    let absolute = absolute_database_path(path)?;
    let resolved_lock_parent = lock_path
        .parent()
        .ok_or_else(repository_unavailable)?
        .canonicalize()
        .map_err(|_| repository_unavailable())?;
    let resolved_database_path = if absolute.exists() {
        absolute
            .canonicalize()
            .map_err(|_| repository_unavailable())?
    } else {
        let file_name = absolute.file_name().ok_or_else(repository_unavailable)?;
        let resolved_database_parent = absolute
            .parent()
            .ok_or_else(repository_unavailable)?
            .canonicalize()
            .map_err(|_| repository_unavailable())?;
        if resolved_database_parent != resolved_lock_parent {
            return Err(integrity::integrity_recovery_required());
        }
        resolved_database_parent.join(file_name)
    };
    let resolved_database_parent = resolved_database_path
        .parent()
        .ok_or_else(repository_unavailable)?;
    if resolved_database_parent != resolved_lock_parent {
        return Err(integrity::integrity_recovery_required());
    }
    Ok(resolved_database_path)
}

fn staging_database_path(path: &Path) -> PathBuf {
    path_with_suffix(path, &format!(".bootstrap-{}.tmp", uuid::Uuid::new_v4()))
}

fn cleanup_paths(
    paths: &[PathBuf],
    cleanup_error: crate::mcp_platform::error::McpPlatformError,
) -> McpPlatformResult<()> {
    let mut failed = false;
    for path in paths {
        if path.exists() && remove_file_for_cleanup(path).is_err() {
            failed = true;
        }
    }
    if failed {
        return Err(cleanup_error);
    }
    Ok(())
}

fn remove_file_for_cleanup(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        const SHARING_VIOLATION: i32 = 32;
        const LOCK_VIOLATION: i32 = 33;
        const ATTEMPTS: usize = 40;
        for attempt in 0..ATTEMPTS {
            match std::fs::remove_file(path) {
                Ok(()) => return Ok(()),
                Err(error)
                    if matches!(
                        error.raw_os_error(),
                        Some(SHARING_VIOLATION | LOCK_VIOLATION)
                    ) && attempt + 1 < ATTEMPTS =>
                {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("cleanup retries always return before exhaustion")
    }
    #[cfg(not(windows))]
    {
        std::fs::remove_file(path)
    }
}

struct StagedDatabaseActivationFailure {
    error: crate::mcp_platform::error::McpPlatformError,
    final_created: bool,
}

fn activate_staged_database(
    staged_path: &Path,
    final_path: &Path,
) -> Result<(), StagedDatabaseActivationFailure> {
    move_file_without_replace_with_state(staged_path, final_path).map_err(|failure| {
        StagedDatabaseActivationFailure {
            error: error(
                McpPlatformErrorCode::RepositoryUnavailable,
                "managed MCP bootstrap activation failed after integrity publication",
            ),
            final_created: failure.final_created,
        }
    })
}

fn fail_closed_after_anchor_reset<T>(
    error: crate::mcp_platform::error::McpPlatformError,
    path_binding: &str,
    cleanups: impl IntoIterator<Item = McpPlatformResult<()>>,
) -> McpPlatformResult<T> {
    if let Err(anchor_error) = integrity::clear_system_anchor_for_trusted_reenrollment(path_binding)
    {
        return fail_after_cleanup(anchor_error, cleanups);
    }
    fail_after_cleanup(error, cleanups)
}

fn bootstrap_cleanup_failed() -> crate::mcp_platform::error::McpPlatformError {
    error(
        McpPlatformErrorCode::RollbackIncomplete,
        "managed MCP bootstrap cleanup failed after activation was rejected",
    )
}

fn activated_database_cleanup_failed() -> crate::mcp_platform::error::McpPlatformError {
    error(
        McpPlatformErrorCode::RollbackIncomplete,
        "managed MCP bootstrap rollback failed after activation was rejected",
    )
}

fn trusted_reenrollment_restore_failed() -> crate::mcp_platform::error::McpPlatformError {
    error(
        McpPlatformErrorCode::RollbackIncomplete,
        "managed MCP trusted re-enrollment rollback failed after anchor reset was rejected",
    )
}

fn restore_moved_paths(paths: &[(PathBuf, PathBuf)]) -> McpPlatformResult<()> {
    let mut restored_paths: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (source, destination) in paths.iter().rev() {
        if source.exists()
            && rename_for_trusted_reenrollment(
                source,
                destination,
                TrustedReenrollmentRenamePhase::Restore,
            )
            .is_err()
        {
            for (restored_source, restored_destination) in restored_paths.iter().rev() {
                if restored_destination.exists() {
                    let _ = rename_for_trusted_reenrollment(
                        restored_destination,
                        restored_source,
                        TrustedReenrollmentRenamePhase::Compensate,
                    );
                }
            }
            return Err(trusted_reenrollment_restore_failed());
        }
        restored_paths.push((source.clone(), destination.clone()));
    }
    Ok(())
}

fn rollback_trusted_reenrollment_bootstrap_failure(
    failure: &FileBackedBootstrapFailure,
    moved_paths: &[(PathBuf, PathBuf)],
    clear_anchor: impl FnOnce() -> McpPlatformResult<()>,
) -> McpPlatformResult<()> {
    let mut rollback_failed = false;
    if let Some((paths, cleanup_error)) = failure.created_final_cleanup() {
        rollback_failed |= cleanup_paths(&paths, cleanup_error).is_err();
    }
    rollback_failed |= clear_anchor().is_err();
    rollback_failed |= restore_moved_paths(moved_paths).is_err();
    if rollback_failed {
        Err(trusted_reenrollment_restore_failed())
    } else {
        Ok(())
    }
}

fn fail_after_cleanup<T>(
    error: crate::mcp_platform::error::McpPlatformError,
    cleanups: impl IntoIterator<Item = McpPlatformResult<()>>,
) -> McpPlatformResult<T> {
    for cleanup in cleanups {
        if let Err(cleanup_error) = cleanup {
            return Err(cleanup_error);
        }
    }
    Err(error)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrustedReenrollmentRenamePhase {
    Move,
    Restore,
    Compensate,
}

fn rename_for_trusted_reenrollment(
    source: &Path,
    destination: &Path,
    phase: TrustedReenrollmentRenamePhase,
) -> std::io::Result<()> {
    #[cfg(test)]
    maybe_fail_trusted_reenrollment_rename(source, destination, phase)?;
    #[cfg(not(test))]
    let _ = phase;
    move_file_without_replace(source, destination)
}

fn move_file_without_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    move_file_without_replace_with_state(source, destination).map_err(|failure| failure.error)
}

struct MoveFileWithoutReplaceFailure {
    error: std::io::Error,
    final_created: bool,
}

fn move_file_without_replace_with_state(
    source: &Path,
    destination: &Path,
) -> Result<(), MoveFileWithoutReplaceFailure> {
    if source == destination {
        return Ok(());
    }
    std::fs::hard_link(source, destination).map_err(|error| MoveFileWithoutReplaceFailure {
        error,
        final_created: false,
    })?;
    #[cfg(test)]
    maybe_fail_move_file_source_unlink(source, destination).map_err(|error| {
        MoveFileWithoutReplaceFailure {
            error,
            final_created: true,
        }
    })?;
    remove_file_for_cleanup(source).map_err(|error| MoveFileWithoutReplaceFailure {
        error,
        final_created: true,
    })
}

#[cfg(test)]
type AfterPreflightBeforeLockHook = Box<dyn FnOnce(&Path) + Send + 'static>;

#[cfg(test)]
struct QueuedPathScopedRegistration<T> {
    id: u64,
    value: T,
}

#[cfg(test)]
struct PathScopedTestRegistrations<T> {
    entries: std::collections::HashMap<
        PathBuf,
        std::collections::VecDeque<QueuedPathScopedRegistration<T>>,
    >,
}

#[cfg(test)]
impl<T> Default for PathScopedTestRegistrations<T> {
    fn default() -> Self {
        Self {
            entries: std::collections::HashMap::new(),
        }
    }
}

#[cfg(test)]
impl<T> PathScopedTestRegistrations<T> {
    fn insert(&mut self, path: PathBuf, value: T) -> u64 {
        let id = next_test_injection_id();
        self.entries
            .entry(path)
            .or_default()
            .push_back(QueuedPathScopedRegistration { id, value });
        id
    }

    fn pop_front(&mut self, path: &Path) -> Option<T> {
        let (registration, remove_entry) = {
            let registrations = self.entries.get_mut(path)?;
            let registration = registrations.pop_front()?;
            let remove_entry = registrations.is_empty();
            (registration, remove_entry)
        };
        if remove_entry {
            self.entries.remove(path);
        }
        Some(registration.value)
    }

    fn remove(&mut self, path: &Path, id: u64) -> Option<T> {
        let (registration, remove_entry) = {
            let registrations = self.entries.get_mut(path)?;
            let index = registrations
                .iter()
                .position(|registration| registration.id == id)?;
            let registration = registrations.remove(index)?;
            let remove_entry = registrations.is_empty();
            (registration, remove_entry)
        };
        if remove_entry {
            self.entries.remove(path);
        }
        Some(registration.value)
    }
}

#[cfg(test)]
type AfterLegacyProviderBindingDatabaseRewriteHook =
    Box<dyn FnOnce() -> McpPlatformResult<()> + Send + 'static>;

#[cfg(test)]
fn after_legacy_provider_binding_database_rewrite_hook() -> &'static std::sync::Mutex<
    std::collections::VecDeque<
        QueuedPathScopedRegistration<AfterLegacyProviderBindingDatabaseRewriteHook>,
    >,
> {
    static HOOKS: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::VecDeque<
                QueuedPathScopedRegistration<AfterLegacyProviderBindingDatabaseRewriteHook>,
            >,
        >,
    > = std::sync::OnceLock::new();
    HOOKS.get_or_init(|| std::sync::Mutex::new(std::collections::VecDeque::new()))
}

#[cfg(test)]
fn install_after_legacy_provider_binding_database_rewrite_hook(
    hook: impl FnOnce() -> McpPlatformResult<()> + Send + 'static,
) -> AfterLegacyProviderBindingDatabaseRewriteHookGuard {
    let id = next_test_injection_id();
    after_legacy_provider_binding_database_rewrite_hook()
        .lock()
        .unwrap()
        .push_back(QueuedPathScopedRegistration {
            id,
            value: Box::new(hook),
        });
    AfterLegacyProviderBindingDatabaseRewriteHookGuard { id }
}

#[cfg(test)]
fn run_after_legacy_provider_binding_database_rewrite_hook() -> McpPlatformResult<()> {
    match after_legacy_provider_binding_database_rewrite_hook()
        .lock()
        .unwrap()
        .pop_front()
    {
        Some(registration) => (registration.value)(),
        None => Ok(()),
    }
}

#[cfg(not(test))]
fn run_after_legacy_provider_binding_database_rewrite_hook() -> McpPlatformResult<()> {
    Ok(())
}

#[cfg(test)]
struct AfterLegacyProviderBindingDatabaseRewriteHookGuard {
    id: u64,
}

#[cfg(test)]
impl Drop for AfterLegacyProviderBindingDatabaseRewriteHookGuard {
    fn drop(&mut self) {
        let mut hooks = after_legacy_provider_binding_database_rewrite_hook()
            .lock()
            .unwrap();
        if let Some(index) = hooks
            .iter()
            .position(|registration| registration.id == self.id)
        {
            hooks.remove(index);
        }
    }
}

#[cfg(test)]
fn after_preflight_before_lock_hook(
) -> &'static std::sync::Mutex<PathScopedTestRegistrations<AfterPreflightBeforeLockHook>> {
    static HOOKS: std::sync::OnceLock<
        std::sync::Mutex<PathScopedTestRegistrations<AfterPreflightBeforeLockHook>>,
    > = std::sync::OnceLock::new();
    HOOKS.get_or_init(|| std::sync::Mutex::new(PathScopedTestRegistrations::default()))
}

#[cfg(test)]
fn install_after_preflight_before_lock_hook(
    path: impl Into<PathBuf>,
    hook: impl FnOnce(&Path) + Send + 'static,
) -> AfterPreflightBeforeLockHookGuard {
    let path = path.into();
    let id = after_preflight_before_lock_hook()
        .lock()
        .unwrap()
        .insert(path.clone(), Box::new(hook));
    AfterPreflightBeforeLockHookGuard { path, id }
}

#[cfg(test)]
fn run_after_preflight_before_lock_hook(path: &Path) {
    if let Some(hook) = after_preflight_before_lock_hook()
        .lock()
        .unwrap()
        .pop_front(path)
    {
        hook(path);
    }
}

#[cfg(test)]
struct AfterPreflightBeforeLockHookGuard {
    path: PathBuf,
    id: u64,
}

#[cfg(test)]
impl Drop for AfterPreflightBeforeLockHookGuard {
    fn drop(&mut self) {
        after_preflight_before_lock_hook()
            .lock()
            .unwrap()
            .remove(&self.path, self.id);
    }
}

#[cfg(test)]
type AfterDatabaseLockAcquiredHook = Box<dyn FnOnce(&Path, &Path) + Send + 'static>;

#[cfg(test)]
fn after_database_lock_acquired_hook(
) -> &'static std::sync::Mutex<PathScopedTestRegistrations<AfterDatabaseLockAcquiredHook>> {
    static HOOKS: std::sync::OnceLock<
        std::sync::Mutex<PathScopedTestRegistrations<AfterDatabaseLockAcquiredHook>>,
    > = std::sync::OnceLock::new();
    HOOKS.get_or_init(|| std::sync::Mutex::new(PathScopedTestRegistrations::default()))
}

#[cfg(test)]
fn install_after_database_lock_acquired_hook(
    path: impl Into<PathBuf>,
    hook: impl FnOnce(&Path, &Path) + Send + 'static,
) -> AfterDatabaseLockAcquiredHookGuard {
    let path = path.into();
    let id = after_database_lock_acquired_hook()
        .lock()
        .unwrap()
        .insert(path.clone(), Box::new(hook));
    AfterDatabaseLockAcquiredHookGuard { path, id }
}

#[cfg(test)]
fn run_after_database_lock_acquired_hook(path: &Path, resolved_path: &Path) {
    if let Some(hook) = after_database_lock_acquired_hook()
        .lock()
        .unwrap()
        .pop_front(path)
    {
        hook(path, resolved_path);
    }
}

#[cfg(test)]
struct AfterDatabaseLockAcquiredHookGuard {
    path: PathBuf,
    id: u64,
}

#[cfg(test)]
impl Drop for AfterDatabaseLockAcquiredHookGuard {
    fn drop(&mut self) {
        after_database_lock_acquired_hook()
            .lock()
            .unwrap()
            .remove(&self.path, self.id);
    }
}

#[cfg(test)]
type AfterActivationBeforeFinalOpenHook =
    Box<dyn FnOnce(&Path, &Path) -> McpPlatformResult<()> + Send + 'static>;

#[cfg(test)]
fn after_activation_before_final_open_hook(
) -> &'static std::sync::Mutex<PathScopedTestRegistrations<AfterActivationBeforeFinalOpenHook>> {
    static HOOKS: std::sync::OnceLock<
        std::sync::Mutex<PathScopedTestRegistrations<AfterActivationBeforeFinalOpenHook>>,
    > = std::sync::OnceLock::new();
    HOOKS.get_or_init(|| std::sync::Mutex::new(PathScopedTestRegistrations::default()))
}

#[cfg(test)]
fn install_after_activation_before_final_open_hook(
    final_path: impl Into<PathBuf>,
    hook: impl FnOnce(&Path, &Path) -> McpPlatformResult<()> + Send + 'static,
) -> AfterActivationBeforeFinalOpenHookGuard {
    let final_path = final_path.into();
    let id = after_activation_before_final_open_hook()
        .lock()
        .unwrap()
        .insert(final_path.clone(), Box::new(hook));
    AfterActivationBeforeFinalOpenHookGuard { final_path, id }
}

#[cfg(test)]
fn run_after_activation_before_final_open_hook(
    final_path: &Path,
    staged_path: &Path,
) -> McpPlatformResult<()> {
    match after_activation_before_final_open_hook()
        .lock()
        .unwrap()
        .pop_front(final_path)
    {
        Some(hook) => hook(final_path, staged_path),
        None => Ok(()),
    }
}

#[cfg(test)]
struct AfterActivationBeforeFinalOpenHookGuard {
    final_path: PathBuf,
    id: u64,
}

#[cfg(test)]
impl Drop for AfterActivationBeforeFinalOpenHookGuard {
    fn drop(&mut self) {
        after_activation_before_final_open_hook()
            .lock()
            .unwrap()
            .remove(&self.final_path, self.id);
    }
}

#[cfg(test)]
type AfterTrustedReenrollmentAnchorClearHook = Box<dyn FnOnce(&Path) + Send + 'static>;

#[cfg(test)]
fn after_trusted_reenrollment_anchor_clear_hook(
) -> &'static std::sync::Mutex<PathScopedTestRegistrations<AfterTrustedReenrollmentAnchorClearHook>>
{
    static HOOKS: std::sync::OnceLock<
        std::sync::Mutex<PathScopedTestRegistrations<AfterTrustedReenrollmentAnchorClearHook>>,
    > = std::sync::OnceLock::new();
    HOOKS.get_or_init(|| std::sync::Mutex::new(PathScopedTestRegistrations::default()))
}

#[cfg(test)]
fn install_after_trusted_reenrollment_anchor_clear_hook(
    database_path: impl Into<PathBuf>,
    hook: impl FnOnce(&Path) + Send + 'static,
) -> AfterTrustedReenrollmentAnchorClearHookGuard {
    let database_path = database_path.into();
    let id = after_trusted_reenrollment_anchor_clear_hook()
        .lock()
        .unwrap()
        .insert(database_path.clone(), Box::new(hook));
    AfterTrustedReenrollmentAnchorClearHookGuard { database_path, id }
}

#[cfg(test)]
fn run_after_trusted_reenrollment_anchor_clear_hook(database_path: &Path) {
    if let Some(hook) = after_trusted_reenrollment_anchor_clear_hook()
        .lock()
        .unwrap()
        .pop_front(database_path)
    {
        hook(database_path);
    }
}

#[cfg(test)]
struct AfterTrustedReenrollmentAnchorClearHookGuard {
    database_path: PathBuf,
    id: u64,
}

#[cfg(test)]
impl Drop for AfterTrustedReenrollmentAnchorClearHookGuard {
    fn drop(&mut self) {
        after_trusted_reenrollment_anchor_clear_hook()
            .lock()
            .unwrap()
            .remove(&self.database_path, self.id);
    }
}

#[cfg(test)]
fn trusted_reenrollment_quarantine_tokens(
) -> &'static std::sync::Mutex<PathScopedTestRegistrations<String>> {
    static TOKENS: std::sync::OnceLock<std::sync::Mutex<PathScopedTestRegistrations<String>>> =
        std::sync::OnceLock::new();
    TOKENS.get_or_init(|| std::sync::Mutex::new(PathScopedTestRegistrations::default()))
}

#[cfg(test)]
fn install_trusted_reenrollment_quarantine_token(
    database_path: impl Into<PathBuf>,
    token: impl Into<String>,
) -> TrustedReenrollmentQuarantineTokenGuard {
    let database_path = database_path.into();
    let id = trusted_reenrollment_quarantine_tokens()
        .lock()
        .unwrap()
        .insert(database_path.clone(), token.into());
    TrustedReenrollmentQuarantineTokenGuard { database_path, id }
}

#[cfg(test)]
struct TrustedReenrollmentQuarantineTokenGuard {
    database_path: PathBuf,
    id: u64,
}

#[cfg(test)]
impl Drop for TrustedReenrollmentQuarantineTokenGuard {
    fn drop(&mut self) {
        trusted_reenrollment_quarantine_tokens()
            .lock()
            .unwrap()
            .remove(&self.database_path, self.id);
    }
}

#[cfg(test)]
#[derive(Clone)]
struct TrustedReenrollmentRenameFailure {
    phase: TrustedReenrollmentRenamePhase,
    source: Option<PathBuf>,
    destination: Option<PathBuf>,
}

#[cfg(test)]
fn trusted_reenrollment_rename_failures() -> &'static std::sync::Mutex<
    std::collections::VecDeque<QueuedPathScopedRegistration<Vec<TrustedReenrollmentRenameFailure>>>,
> {
    static FAILURES: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::VecDeque<
                QueuedPathScopedRegistration<Vec<TrustedReenrollmentRenameFailure>>,
            >,
        >,
    > = std::sync::OnceLock::new();
    FAILURES.get_or_init(|| std::sync::Mutex::new(std::collections::VecDeque::new()))
}

#[cfg(test)]
fn install_trusted_reenrollment_rename_failures(
    failures: Vec<TrustedReenrollmentRenameFailure>,
) -> TrustedReenrollmentRenameFailureGuard {
    let id = next_test_injection_id();
    trusted_reenrollment_rename_failures()
        .lock()
        .unwrap()
        .push_back(QueuedPathScopedRegistration {
            id,
            value: failures,
        });
    TrustedReenrollmentRenameFailureGuard { id }
}

#[cfg(test)]
fn maybe_fail_trusted_reenrollment_rename(
    source: &Path,
    destination: &Path,
    phase: TrustedReenrollmentRenamePhase,
) -> std::io::Result<()> {
    let mut failures = trusted_reenrollment_rename_failures().lock().unwrap();
    for registered_failures in failures.iter_mut() {
        if let Some(index) = registered_failures.value.iter().position(|failure| {
            failure.phase == phase
                && failure
                    .source
                    .as_ref()
                    .is_none_or(|expected| expected == source)
                && failure
                    .destination
                    .as_ref()
                    .is_none_or(|expected| expected == destination)
        }) {
            registered_failures.value.remove(index);
            return Err(std::io::Error::other(
                "managed MCP trusted re-enrollment rename failed for testing",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
struct TrustedReenrollmentRenameFailureGuard {
    id: u64,
}

#[cfg(test)]
impl Drop for TrustedReenrollmentRenameFailureGuard {
    fn drop(&mut self) {
        let mut failures = trusted_reenrollment_rename_failures().lock().unwrap();
        if let Some(index) = failures
            .iter()
            .position(|registered_failures| registered_failures.id == self.id)
        {
            failures.remove(index);
        }
    }
}

#[cfg(test)]
type MoveFileSourceUnlinkFailureHook =
    Box<dyn FnOnce(&Path, &Path) -> std::io::Result<()> + Send + 'static>;

#[cfg(test)]
fn move_file_source_unlink_failure_hooks(
) -> &'static std::sync::Mutex<PathScopedTestRegistrations<MoveFileSourceUnlinkFailureHook>> {
    static HOOKS: std::sync::OnceLock<
        std::sync::Mutex<PathScopedTestRegistrations<MoveFileSourceUnlinkFailureHook>>,
    > = std::sync::OnceLock::new();
    HOOKS.get_or_init(|| std::sync::Mutex::new(PathScopedTestRegistrations::default()))
}

#[cfg(test)]
fn install_move_file_source_unlink_failure_hook(
    source_path: impl Into<PathBuf>,
    hook: impl FnOnce(&Path, &Path) -> std::io::Result<()> + Send + 'static,
) -> MoveFileSourceUnlinkFailureHookGuard {
    let source_path = source_path.into();
    let id = move_file_source_unlink_failure_hooks()
        .lock()
        .unwrap()
        .insert(source_path.clone(), Box::new(hook));
    MoveFileSourceUnlinkFailureHookGuard { source_path, id }
}

#[cfg(test)]
fn maybe_fail_move_file_source_unlink(source: &Path, destination: &Path) -> std::io::Result<()> {
    match move_file_source_unlink_failure_hooks()
        .lock()
        .unwrap()
        .pop_front(source)
    {
        Some(hook) => hook(source, destination),
        None => Ok(()),
    }
}

#[cfg(test)]
struct MoveFileSourceUnlinkFailureHookGuard {
    source_path: PathBuf,
    id: u64,
}

#[cfg(test)]
impl Drop for MoveFileSourceUnlinkFailureHookGuard {
    fn drop(&mut self) {
        move_file_source_unlink_failure_hooks()
            .lock()
            .unwrap()
            .remove(&self.source_path, self.id);
    }
}

#[cfg(test)]
fn next_test_injection_id() -> u64 {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn database_path_binding(path: &Path) -> McpPlatformResult<String> {
    database_path_binding_from_resolved_path(&absolute_path_for_binding(path)?)
}

fn database_path_binding_from_resolved_path(path: &Path) -> McpPlatformResult<String> {
    let mut hasher = Sha256::new();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        hash_component(&mut hasher, b"lumina.mcp-platform.path-binding.unix-v1");
        hash_component(&mut hasher, path.as_os_str().as_bytes());
    }
    #[cfg(windows)]
    {
        let normalized = path
            .to_str()
            .ok_or_else(|| {
                error(
                    McpPlatformErrorCode::InvalidRequest,
                    "database path cannot be represented without loss on this platform",
                )
            })?
            .replace('/', "\\");
        hash_component(&mut hasher, b"lumina.mcp-platform.path-binding.windows-v1");
        hash_component(&mut hasher, normalized.as_bytes());
    }
    #[cfg(not(any(unix, windows)))]
    {
        let normalized = path.to_str().ok_or_else(|| {
            error(
                McpPlatformErrorCode::InvalidRequest,
                "database path cannot be represented without loss on this platform",
            )
        })?;
        hash_component(&mut hasher, b"lumina.mcp-platform.path-binding.portable-v1");
        hash_component(&mut hasher, normalized.as_bytes());
    }
    Ok(crate::utils::bytes_to_hex(hasher.finalize()))
}

fn absolute_database_path(path: &Path) -> McpPlatformResult<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .map_err(|_| repository_unavailable())?
            .join(path))
    }
}

// Resolve against the nearest existing ancestor so integrity preflight can run
// before creating parent directories or lock files.
fn absolute_path_for_binding(path: &Path) -> McpPlatformResult<PathBuf> {
    let absolute = absolute_database_path(path)?;
    let mut suffix = Vec::new();
    let mut cursor = absolute.as_path();
    while !cursor.exists() {
        let component = cursor.file_name().ok_or_else(repository_unavailable)?;
        suffix.push(component.to_os_string());
        cursor = cursor.parent().ok_or_else(repository_unavailable)?;
    }
    let mut resolved = cursor
        .canonicalize()
        .map_err(|_| repository_unavailable())?;
    for component in suffix.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

async fn load_schema_version_state(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<(bool, i64)> {
    let schema_exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_version')",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    let current = if schema_exists {
        sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(version) FROM schema_version")
            .fetch_one(&mut **tx)
            .await
            .map_err(map_sqlx)?
            .unwrap_or(0)
    } else {
        0
    };
    Ok((schema_exists, current))
}

fn expected_v15_schema_fence_pending() -> integrity::SchemaCompatibilityFence {
    integrity::SchemaCompatibilityFence::projection_witness_v2_v15_pending()
}

fn expected_v15_schema_fence_active() -> integrity::SchemaCompatibilityFence {
    integrity::SchemaCompatibilityFence::projection_witness_v2_v15_active()
}

fn validate_v15_schema_fence(signer: &dyn IntegritySigner) -> McpPlatformResult<()> {
    if !matches!(
        signer.assurance(),
        integrity::AnchorProviderAssurance::SystemKeyring
    ) {
        return Ok(());
    }
    match signer.stored_schema_compatibility_fence()? {
        Some(fence) if fence == expected_v15_schema_fence_active() => Ok(()),
        _ => Err(integrity_error()),
    }
}

async fn preflight_migration_state(
    pool: &Pool<Sqlite>,
    signer: &dyn IntegritySigner,
) -> McpPlatformResult<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(map_sqlx)?;
    let (_, current) = load_schema_version_state(&mut tx).await?;
    if current >= 29 {
        migrations::validate_governed_source_refresh_schema(&mut tx).await?;
    }
    let projection_table_exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'projection_mutations')",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(map_sqlx)?;
    if projection_table_exists && current < 15 {
        match migrations::classify_projection_mutations_schema(&mut tx).await {
            Ok(migrations::ProjectionMutationsTableShape::WitnessV2) if current == 14 => {
                if matches!(
                    signer.assurance(),
                    integrity::AnchorProviderAssurance::SystemKeyring
                ) {
                    let _ = validate_v15_schema_fence(signer);
                }
                return Err(integrity_error());
            }
            Ok(migrations::ProjectionMutationsTableShape::WitnessV2) => {
                return Err(integrity_error());
            }
            Ok(migrations::ProjectionMutationsTableShape::LegacyV14) => {}
            Err(error) => return Err(error),
        }
    }
    tx.rollback().await.map_err(map_sqlx)?;
    Ok(())
}

async fn ensure_schema_compatibility_fence(
    pool: &Pool<Sqlite>,
    signer: &dyn IntegritySigner,
) -> McpPlatformResult<()> {
    if !matches!(
        signer.assurance(),
        integrity::AnchorProviderAssurance::SystemKeyring
    ) {
        return Ok(());
    }
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(map_sqlx)?;
    let (_, current) = load_schema_version_state(&mut tx).await?;
    if current < 15 {
        tx.rollback().await.map_err(map_sqlx)?;
        return Ok(());
    }
    migrations::validate_projection_mutations_schema(&mut tx).await?;
    migrations::validate_governed_source_refresh_schema(&mut tx).await?;
    let checkpoint = verify_integrity_transaction(&mut tx, signer).await?;
    tx.rollback().await.map_err(map_sqlx)?;
    signer.ensure_schema_compatibility_fence(&checkpoint, &expected_v15_schema_fence_active())
}

async fn migrate(pool: &Pool<Sqlite>) -> McpPlatformResult<bool> {
    migrate_through_version(pool, migrations::CURRENT_SCHEMA_VERSION).await
}

async fn load_current_schema_version(pool: &Pool<Sqlite>) -> McpPlatformResult<i64> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(map_sqlx)?;
    let (_, current) = load_schema_version_state(&mut tx).await?;
    tx.rollback().await.map_err(map_sqlx)?;
    Ok(current)
}

async fn validate_current_v15_projection_schema(pool: &Pool<Sqlite>) -> McpPlatformResult<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(map_sqlx)?;
    migrations::validate_projection_mutations_schema(&mut tx).await?;
    tx.rollback().await.map_err(map_sqlx)
}

async fn append_integrity_checkpoint_without_external_publish(
    transaction: &mut Transaction<'_, Sqlite>,
    signer: &dyn IntegritySigner,
    current: &integrity::AnchorCheckpoint,
) -> McpPlatformResult<integrity::AnchorCheckpoint> {
    let state_digest = security_state_digest(transaction).await?;
    let next = next_checkpoint(current, &state_digest);
    let commit_mac = sign_checkpoint(signer, &next, &current.root, &state_digest)?;
    sqlx::query(
        "INSERT INTO integrity_commits(sequence,instance_id,key_epoch,provider_id,parent_root,state_digest,root,commit_mac) VALUES (?,?,?,?,?,?,?,?)",
    )
    .bind(i64::try_from(next.sequence).map_err(|_| integrity::integrity_recovery_required())?)
    .bind(&next.identity.instance_id)
    .bind(
        i64::try_from(next.identity.key_epoch)
            .map_err(|_| integrity::integrity_recovery_required())?,
    )
    .bind(&next.identity.provider_id)
    .bind(&current.root)
    .bind(&state_digest)
    .bind(&next.root)
    .bind(&commit_mac)
    .execute(&mut **transaction)
    .await
    .map_err(map_sqlx)?;
    let updated = sqlx::query(
        "UPDATE integrity_metadata SET sequence=?,root=?,state_digest=?,commit_mac=?,status='active' WHERE singleton=1 AND instance_id=? AND key_epoch=? AND provider_id=? AND sequence=? AND root=? AND status='active'",
    )
    .bind(i64::try_from(next.sequence).map_err(|_| integrity::integrity_recovery_required())?)
    .bind(&next.root)
    .bind(&state_digest)
    .bind(&commit_mac)
    .bind(&next.identity.instance_id)
    .bind(
        i64::try_from(next.identity.key_epoch)
            .map_err(|_| integrity::integrity_recovery_required())?,
    )
    .bind(&next.identity.provider_id)
    .bind(i64::try_from(current.sequence).map_err(|_| integrity::integrity_recovery_required())?)
    .bind(&current.root)
    .execute(&mut **transaction)
    .await
    .map_err(map_sqlx)?;
    if updated.rows_affected() != 1 {
        return Err(integrity::integrity_recovery_required());
    }
    Ok(next)
}

async fn migrate_v15_with_internal_integrity_commit(
    pool: &Pool<Sqlite>,
    signer: &dyn IntegritySigner,
    expected_current: &integrity::AnchorCheckpoint,
) -> McpPlatformResult<integrity::AnchorCheckpoint> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(map_sqlx)?;
    let (_, current) = load_schema_version_state(&mut tx).await?;
    if current >= 15 {
        tx.rollback().await.map_err(map_sqlx)?;
        return Err(integrity::integrity_recovery_required());
    }
    let verified_current = verify_integrity_transaction(&mut tx, signer).await?;
    if verified_current != *expected_current {
        tx.rollback().await.map_err(map_sqlx)?;
        return Err(integrity::integrity_recovery_required());
    }
    migrations::apply_v15(&mut tx).await?;
    sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (15, 0)")
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
    migrations::validate_projection_mutations_schema(&mut tx).await?;
    let next =
        append_integrity_checkpoint_without_external_publish(&mut tx, signer, &verified_current)
            .await?;
    tx.commit().await.map_err(map_sqlx)?;
    Ok(next)
}

async fn complete_system_keyring_schema_guarded_open(
    pool: &Pool<Sqlite>,
    signer: &dyn IntegritySigner,
    allow_bootstrap: bool,
    expected_path_binding: Option<&str>,
) -> McpPlatformResult<()> {
    let current = load_current_schema_version(pool).await?;
    if current < 15 {
        initialize_or_verify_integrity(pool, signer, allow_bootstrap, expected_path_binding)
            .await?;
        let checkpoint = verified_external_checkpoint(pool, signer, expected_path_binding).await?;
        match signer.stored_schema_compatibility_fence()? {
            None => signer.ensure_schema_compatibility_fence(
                &checkpoint,
                &expected_v15_schema_fence_pending(),
            )?,
            Some(fence) if fence == expected_v15_schema_fence_pending() => {}
            _ => return Err(integrity_error()),
        }
        let next = migrate_v15_with_internal_integrity_commit(pool, signer, &checkpoint).await?;
        signer.publish(Some(&checkpoint), &next)?;
        signer.ensure_schema_compatibility_fence(&next, &expected_v15_schema_fence_active())?;
    } else {
        validate_current_v15_projection_schema(pool).await?;

        match signer.stored_schema_compatibility_fence()? {
            Some(fence) if fence == expected_v15_schema_fence_pending() => {
                if initialize_or_verify_integrity(pool, signer, false, expected_path_binding)
                    .await
                    .is_ok()
                {
                    let checkpoint =
                        verified_external_checkpoint(pool, signer, expected_path_binding).await?;
                    signer.ensure_schema_compatibility_fence(
                        &checkpoint,
                        &expected_v15_schema_fence_active(),
                    )?;
                } else {
                    let external = signer
                        .checkpoint()?
                        .ok_or_else(integrity::integrity_recovery_required)?;
                    let current_checkpoint = verify_post_migration_pending_v15_checkpoint(
                        pool,
                        signer,
                        expected_path_binding,
                        &external,
                    )
                    .await?;
                    signer.publish(Some(&external), &current_checkpoint)?;
                    signer.ensure_schema_compatibility_fence(
                        &current_checkpoint,
                        &expected_v15_schema_fence_active(),
                    )?;
                }
            }
            None => {
                initialize_or_verify_integrity(pool, signer, false, expected_path_binding).await?;
                let checkpoint =
                    verified_external_checkpoint(pool, signer, expected_path_binding).await?;
                signer.ensure_schema_compatibility_fence(
                    &checkpoint,
                    &expected_v15_schema_fence_pending(),
                )?;
                signer.ensure_schema_compatibility_fence(
                    &checkpoint,
                    &expected_v15_schema_fence_active(),
                )?;
            }
            Some(fence) if fence == expected_v15_schema_fence_active() => {
                initialize_or_verify_integrity(pool, signer, false, expected_path_binding).await?;
                let checkpoint =
                    verified_external_checkpoint(pool, signer, expected_path_binding).await?;
                signer.ensure_schema_compatibility_fence(
                    &checkpoint,
                    &expected_v15_schema_fence_active(),
                )?;
            }
            Some(_) => return Err(integrity_error()),
        }
    }

    let current = load_current_schema_version(pool).await?;
    if current < migrations::CURRENT_SCHEMA_VERSION {
        migrate_through_version_with_integrity_commit(
            pool,
            signer,
            expected_path_binding,
            migrations::CURRENT_SCHEMA_VERSION,
        )
        .await?;
    }
    ensure_schema_compatibility_fence(pool, signer).await
}

async fn migrate_through_version(
    pool: &Pool<Sqlite>,
    target_version: i64,
) -> McpPlatformResult<bool> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(map_sqlx)?;
    let (schema_exists, current) = load_schema_version_state(&mut tx).await?;
    migrate_schema_in_transaction(&mut tx, schema_exists, current, target_version).await?;
    tx.commit().await.map_err(map_sqlx)?;
    Ok(!schema_exists)
}

async fn migrate_schema_in_transaction(
    tx: &mut Transaction<'_, Sqlite>,
    schema_exists: bool,
    current: i64,
    target_version: i64,
) -> McpPlatformResult<()> {
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
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    }
    if current < 1 && target_version >= 1 {
        migrations::apply_v1(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (1, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 2 && target_version >= 2 {
        migrations::apply_v2(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (2, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 3 && target_version >= 3 {
        migrations::apply_v3(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (3, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 4 && target_version >= 4 {
        migrations::apply_v4(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (4, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 5 && target_version >= 5 {
        migrations::apply_v5(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (5, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 6 && target_version >= 6 {
        migrations::apply_v6(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (6, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 7 && target_version >= 7 {
        migrations::apply_v7(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (7, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 8 && target_version >= 8 {
        migrations::apply_v8(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (8, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 9 && target_version >= 9 {
        migrations::apply_v9(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (9, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 10 && target_version >= 10 {
        migrations::apply_v10(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (10, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 11 && target_version >= 11 {
        migrations::apply_v11(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (11, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 12 && target_version >= 12 {
        migrations::apply_v12(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (12, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 13 && target_version >= 13 {
        migrations::apply_v13(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (13, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 14 && target_version >= 14 {
        migrations::apply_v14(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (14, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 15 && target_version >= 15 {
        migrations::apply_v15(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (15, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 16 && target_version >= 16 {
        migrations::apply_v16(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (16, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 17 && target_version >= 17 {
        migrations::apply_v17(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (17, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 18 && target_version >= 18 {
        migrations::apply_v18(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (18, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 19 && target_version >= 19 {
        migrations::apply_v19(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (19, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 20 && target_version >= 20 {
        migrations::apply_v20(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (20, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 21 && target_version >= 21 {
        migrations::apply_v21(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (21, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 22 && target_version >= 22 {
        migrations::apply_v22(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (22, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 23 && target_version >= 23 {
        migrations::apply_v23(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (23, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 24 && target_version >= 24 {
        migrations::apply_v24(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (24, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 25 && target_version >= 25 {
        migrations::apply_v25(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (25, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 26 && target_version >= 26 {
        migrations::validate_remote_inspection_v25_schema(tx).await?;
        match migrations::inspect_remote_inspection_v26_schema_state(tx).await? {
            migrations::RemoteInspectionV26SchemaState::Absent => {
                migrations::apply_v26(tx).await?;
                migrations::validate_remote_inspection_v26_schema(tx).await?;
                sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (26, 0)")
                    .execute(&mut **tx)
                    .await
                    .map_err(map_sqlx)?;
            }
            migrations::RemoteInspectionV26SchemaState::Complete
            | migrations::RemoteInspectionV26SchemaState::PartialOrInvalid => {
                return Err(integrity_error());
            }
        }
    }
    if current < 27 && target_version >= 27 {
        migrations::apply_v27(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (27, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 28 && target_version >= 28 {
        migrations::apply_v28(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (28, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 29 && target_version >= 29 {
        migrations::apply_v29(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (29, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 30 && target_version >= 30 {
        migrations::apply_v30(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (30, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 31 && target_version >= 31 {
        migrations::apply_v31(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (31, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 32 && target_version >= 32 {
        migrations::apply_v32(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (32, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 33 && target_version >= 33 {
        migrations::apply_v33(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (33, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 34 && target_version >= 34 {
        migrations::apply_v34(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (34, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 35 && target_version >= 35 {
        migrations::apply_v35(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (35, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 36 && target_version >= 36 {
        if target_version >= 37 {
            migrations::apply_v37_compatible_v36(tx).await?;
        } else {
            migrations::apply_v36(tx).await?;
        }
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (36, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 37 && target_version >= 37 {
        migrations::apply_v37(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (37, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 38 && target_version >= 38 {
        migrations::apply_v38(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (38, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 39 && target_version >= 39 {
        migrations::apply_v39(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (39, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 40 && target_version >= 40 {
        migrations::apply_v40(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (40, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 41 && target_version >= 41 {
        migrations::apply_v41(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (41, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 42 && target_version >= 42 {
        migrations::apply_v42(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (42, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 43 && target_version >= 43 {
        migrations::apply_v43(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (43, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 44 && target_version >= 44 {
        migrations::apply_v44(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (44, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 45 && target_version >= 45 {
        migrations::apply_v45(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (45, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 46 && target_version >= 46 {
        migrations::apply_v46(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (46, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if current < 47 && target_version >= 47 {
        migrations::apply_v47(tx).await?;
        sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (47, 0)")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    if target_version >= 29 {
        migrations::validate_governed_source_refresh_schema(tx).await?;
    }
    if target_version >= 15 {
        migrations::validate_projection_mutations_schema(tx).await?;
    }
    Ok(())
}

async fn migrate_through_version_with_integrity_commit(
    pool: &Pool<Sqlite>,
    signer: &dyn IntegritySigner,
    expected_path_binding: Option<&str>,
    target_version: i64,
) -> McpPlatformResult<bool> {
    let expected_current =
        verified_external_checkpoint(pool, signer, expected_path_binding).await?;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(map_sqlx)?;
    let (schema_exists, current) = load_schema_version_state(&mut tx).await?;
    let verified_current = verify_integrity_transaction(&mut tx, signer).await?;
    if verified_current != expected_current {
        tx.rollback().await.map_err(map_sqlx)?;
        return Err(integrity::integrity_recovery_required());
    }
    migrate_schema_in_transaction(&mut tx, schema_exists, current, target_version).await?;
    let next =
        append_integrity_checkpoint_without_external_publish(&mut tx, signer, &verified_current)
            .await?;
    tx.commit().await.map_err(map_sqlx)?;
    signer.publish(Some(&verified_current), &next)?;
    Ok(!schema_exists)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use async_trait::async_trait;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use ed25519_dalek::{Signer, SigningKey};
    use sha2::{Digest as _, Sha256};

    use crate::mcp_platform::adapters::plan_for_manifest;
    use crate::mcp_platform::health::ProductionHealthCheckAdapter;
    use crate::mcp_platform::lifecycle::{
        ConfigAuthRequirementResolver, ConfigProjectionSink, CoreTransportProjectionAdapter,
        EmptyHostIntegrationAdapter, LifecyclePorts, ProjectionCommitPlan, ProjectionSink,
        ProjectionSinkAtomicProof, ProjectionSnapshot, SafeRegistrationEffectAdapter,
    };
    use crate::mcp_platform::plan::ConnectionProjection;
    use crate::mcp_platform::repository::{
        ActivateManagedInstallation, AuditEventType, AuditPayload, ProjectionAuthorization,
        ProjectionSinkAtomicProofKind, ProjectionSinkCommitReceipt, PutOwnedProjection,
        SaveGovernedCatalogEntry, StageManagedInstallation, StepTransition, TaskStepRecord,
        TaskTransition,
    };
    use crate::mcp_platform::task::{
        CompensationDescriptor, RedactedErrorCode, StepEvidence, TaskStatus, TaskStepStatus,
    };
    use crate::mcp_platform::{
        parse_manifest, ConfirmationEvidence, CreateTask, ManifestProof, ManifestRecord,
        ManifestSourceMetadata, OriginProvenance, PlanOperation, PlanTarget, PolicyContext,
        SavePlan, SignatureEvidence, SourceImportKind, SourceRef, TaskRecord, TrustTier,
        UpdateChannel, VerifiedSourceDocumentRef,
    };

    use super::*;

    const REMOTE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../documentation/static/schemas/examples/remote-http.json"
    ));
    const NPM: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../documentation/static/schemas/examples/npm-package.json"
    ));
    #[cfg(feature = "system-keyring")]
    const INVALID_SYSTEM_ANCHOR_PAYLOAD_CASE_COUNT: usize = 25;

    async fn insert_v32_planned_lifecycle_impact(
        pool: &Pool<Sqlite>,
        impact_digest: &str,
        plan_digest: &str,
        managed_mcp_id: &str,
    ) {
        sqlx::query(
            "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'plan-v32',?,?,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','archive',CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)+60000,1,?,?,'planned',NULL,NULL,1,1)",
        )
        .bind(impact_digest)
        .bind(plan_digest)
        .bind(managed_mcp_id)
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .execute(pool)
        .await
        .unwrap();
    }

    fn profile_lifecycle_test_expiry_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 60_000
    }

    fn plan(
        fixture: &str,
        trust_tier: TrustTier,
    ) -> (
        crate::mcp_platform::VerifiedManifest,
        crate::mcp_platform::InstallationPlan,
    ) {
        let manifest = parse_manifest(fixture.as_bytes()).unwrap();
        let context = PolicyContext::new(trust_tier, PlanOperation::Register);
        let plan = plan_for_manifest(&manifest, &context).unwrap();
        (manifest, plan)
    }

    fn test_ports() -> LifecyclePorts {
        LifecyclePorts {
            registration: Arc::new(SafeRegistrationEffectAdapter::default()),
            host_integration: Arc::new(EmptyHostIntegrationAdapter),
            transport: Arc::new(CoreTransportProjectionAdapter),
            auth: Arc::new(ConfigAuthRequirementResolver),
            health: Arc::new(ProductionHealthCheckAdapter::default()),
            projection_sink: Arc::new(ConfigProjectionSink::default()),
        }
    }

    fn v42_fenced_binding_digest(fields: &[&[u8]]) -> String {
        let mut hasher = Sha256::new();
        for field in fields {
            hasher.update((field.len() as u64).to_be_bytes());
            hasher.update(field);
        }
        crate::utils::bytes_to_hex(hasher.finalize())
    }

    fn v42_fenced_command_binding(
        repository_instance_id: &str,
        repository_path_binding: &str,
        repository_key_epoch: i64,
        sink_identity: &str,
        sink_version: &str,
        sink_authority: &str,
        grant_id: &str,
        effect_id: &str,
        fence_epoch: i64,
        target_digest: &str,
        canonical_digest: &str,
    ) -> String {
        v42_fenced_binding_digest(&[
            b"lumina.mcp-platform.fenced-projection-command-v42",
            repository_instance_id.as_bytes(),
            repository_path_binding.as_bytes(),
            repository_key_epoch.to_string().as_bytes(),
            sink_identity.as_bytes(),
            sink_version.as_bytes(),
            sink_authority.as_bytes(),
            b"global",
            grant_id.as_bytes(),
            effect_id.as_bytes(),
            fence_epoch.to_string().as_bytes(),
            target_digest.as_bytes(),
            canonical_digest.as_bytes(),
        ])
    }

    fn v42_fenced_receipt_binding(
        receipt_id: &str,
        sink_identity: &str,
        sink_version: &str,
        sink_authority: &str,
        grant_id: &str,
        effect_id: &str,
        fence_epoch: i64,
        target_digest: &str,
        command_binding: &str,
    ) -> String {
        v42_fenced_binding_digest(&[
            b"lumina.mcp-platform.fenced-projection-receipt-v42",
            receipt_id.as_bytes(),
            sink_identity.as_bytes(),
            sink_version.as_bytes(),
            sink_authority.as_bytes(),
            grant_id.as_bytes(),
            effect_id.as_bytes(),
            fence_epoch.to_string().as_bytes(),
            target_digest.as_bytes(),
            command_binding.as_bytes(),
        ])
    }

    struct PublishFailingSigner {
        inner: Arc<dyn IntegritySigner>,
    }

    impl PublishFailingSigner {
        fn new(inner: Arc<dyn IntegritySigner>) -> Arc<dyn IntegritySigner> {
            Arc::new(Self { inner })
        }
    }

    struct PublishThenErrorSigner {
        inner: Arc<dyn IntegritySigner>,
    }

    impl PublishThenErrorSigner {
        fn new(inner: Arc<dyn IntegritySigner>) -> Arc<dyn IntegritySigner> {
            Arc::new(Self { inner })
        }
    }

    impl integrity::AnchorProvider for PublishThenErrorSigner {
        fn provider_id(&self) -> &str {
            self.inner.provider_id()
        }

        fn assurance(&self) -> integrity::AnchorProviderAssurance {
            self.inner.assurance()
        }

        fn health(&self) -> integrity::AnchorProviderHealth {
            self.inner.health()
        }

        fn identity(&self) -> McpPlatformResult<integrity::AnchorIdentity> {
            self.inner.identity()
        }

        fn checkpoint(&self) -> McpPlatformResult<Option<integrity::AnchorCheckpoint>> {
            self.inner.checkpoint()
        }

        fn publish(
            &self,
            expected: Option<&integrity::AnchorCheckpoint>,
            next: &integrity::AnchorCheckpoint,
        ) -> McpPlatformResult<()> {
            self.inner.publish(expected, next)?;
            Err(crate::mcp_platform::error::McpPlatformError::new(
                McpPlatformErrorCode::IntegrityUnavailable,
                "managed MCP integrity publish succeeded before failing for testing",
            ))
        }

        fn sign(&self, domain: &str, canonical_payload: &[u8]) -> McpPlatformResult<String> {
            self.inner.sign(domain, canonical_payload)
        }

        fn matches_expected_path_binding(&self, expected: &str, actual: &str) -> bool {
            self.inner.matches_expected_path_binding(expected, actual)
        }
    }

    impl integrity::AnchorProvider for PublishFailingSigner {
        fn provider_id(&self) -> &str {
            self.inner.provider_id()
        }

        fn assurance(&self) -> integrity::AnchorProviderAssurance {
            self.inner.assurance()
        }

        fn health(&self) -> integrity::AnchorProviderHealth {
            self.inner.health()
        }

        fn identity(&self) -> McpPlatformResult<integrity::AnchorIdentity> {
            self.inner.identity()
        }

        fn checkpoint(&self) -> McpPlatformResult<Option<integrity::AnchorCheckpoint>> {
            self.inner.checkpoint()
        }

        fn publish(
            &self,
            _expected: Option<&integrity::AnchorCheckpoint>,
            _next: &integrity::AnchorCheckpoint,
        ) -> McpPlatformResult<()> {
            Err(crate::mcp_platform::error::McpPlatformError::new(
                McpPlatformErrorCode::IntegrityUnavailable,
                "managed MCP integrity publish failed for testing",
            ))
        }

        fn sign(&self, domain: &str, canonical_payload: &[u8]) -> McpPlatformResult<String> {
            self.inner.sign(domain, canonical_payload)
        }

        fn matches_expected_path_binding(&self, expected: &str, actual: &str) -> bool {
            self.inner.matches_expected_path_binding(expected, actual)
        }
    }

    async fn sqlite_tables(path: &Path) -> Vec<String> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let tables = sqlx::query_scalar::<_, String>(
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        pool.close().await;
        tables
    }

    fn assert_lock_contention(error: std::io::Error) {
        assert!(
            error.kind() == std::io::ErrorKind::WouldBlock
                || matches!(error.raw_os_error(), Some(11 | 32 | 33 | 35 | 36)),
            "unexpected lock contention error: {error:?}"
        );
    }

    fn assert_lock_sidecar_released(lock_path: &Path) {
        assert!(lock_path.exists());
        let released = OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();
        released.try_lock_exclusive().unwrap();
        released.unlock().unwrap();
    }

    fn blocking_recv<T>(receiver: std::sync::mpsc::Receiver<T>) -> T {
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("timed out waiting for test synchronization")
    }

    #[cfg(unix)]
    fn create_parent_link(link: &Path, target: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    #[cfg(windows)]
    fn create_parent_link(link: &Path, target: &Path) {
        std::os::windows::fs::symlink_dir(target, link).unwrap();
    }

    #[cfg(feature = "system-keyring")]
    async fn seed_trusted_reenrollment_repository(path: &Path, managed_mcp_id: &str, mcp_id: &str) {
        let repository = SqliteMcpPlatformRepository::open_path(path).await.unwrap();
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: managed_mcp_id.to_string(),
                    mcp_id: mcp_id.to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository.close().await;
    }

    fn seed_trusted_reenrollment_sidecars(path: &Path) {
        for (suffix, contents) in [
            ("-journal", &b"journal"[..]),
            ("-wal", &b"wal"[..]),
            ("-shm", &b"shm"[..]),
        ] {
            let sidecar = path_with_suffix(path, suffix);
            std::fs::write(&sidecar, contents).unwrap();
            let metadata = std::fs::metadata(&sidecar);
            assert!(
                metadata.is_ok(),
                "trusted reenrollment sidecar metadata failed: suffix={suffix}, error_kind={:?}, raw_os_error={:?}",
                metadata.as_ref().err().map(|error| error.kind()),
                metadata.as_ref().err().and_then(|error| error.raw_os_error()),
            );
        }
    }

    fn assert_trusted_reenrollment_sidecars_exist(path: &Path) {
        for suffix in ["-journal", "-wal", "-shm"] {
            assert!(path_with_suffix(path, suffix).exists());
        }
    }

    fn assert_trusted_reenrollment_sidecars_absent(path: &Path) {
        for suffix in ["-journal", "-wal", "-shm"] {
            assert!(!path_with_suffix(path, suffix).exists());
        }
    }

    fn assert_file_contents(path: &Path, expected: &[u8]) {
        assert_eq!(std::fs::read(path).unwrap(), expected);
    }

    fn assert_no_trusted_reenrollment_quarantine(directory: &Path, path: &Path) {
        let quarantine_prefix = format!(
            "{}.quarantine-",
            path.file_name().unwrap().to_string_lossy()
        );
        assert!(!std::fs::read_dir(directory)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(&quarantine_prefix)));
    }

    #[cfg(feature = "system-keyring")]
    fn mutate_system_anchor_payload(
        valid_payload: &serde_json::Value,
        mutate: impl FnOnce(&mut serde_json::Value),
    ) -> String {
        let mut payload = valid_payload.clone();
        mutate(&mut payload);
        serde_json::to_string(&payload).unwrap()
    }

    #[cfg(feature = "system-keyring")]
    fn invalid_system_anchor_payloads(
        valid_payload: &serde_json::Value,
        raw_valid_payload: &str,
    ) -> Vec<(&'static str, String)> {
        vec![
            (
                "audit-v2-like-wrong-version-evil-provider",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["format_version"] = serde_json::json!(1);
                    payload["checkpoint"]["identity"]["provider_id"] = serde_json::json!("evil");
                }),
            ),
            (
                "audit-v2-like-missing-provider",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["checkpoint"]["identity"]
                        .as_object_mut()
                        .unwrap()
                        .remove("provider_id");
                }),
            ),
            (
                "missing-format-version",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload.as_object_mut().unwrap().remove("format_version");
                }),
            ),
            (
                "format-version-string",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["format_version"] = serde_json::json!("2");
                }),
            ),
            (
                "format-version-4",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["format_version"] = serde_json::json!(4);
                }),
            ),
            (
                "provider-evil",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["checkpoint"]["identity"]["provider_id"] = serde_json::json!("evil");
                }),
            ),
            (
                "provider-swapped-to-in-memory",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["checkpoint"]["identity"]["provider_id"] =
                        serde_json::json!("in-memory-test");
                }),
            ),
            (
                "provider-empty",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["checkpoint"]["identity"]["provider_id"] = serde_json::json!("");
                }),
            ),
            (
                "provider-nul",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["checkpoint"]["identity"]["provider_id"] =
                        serde_json::json!("system-keyring\u{0000}evil");
                }),
            ),
            (
                "provider-unicode",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["checkpoint"]["identity"]["provider_id"] = serde_json::json!("供应商");
                }),
            ),
            (
                "provider-oversize",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["checkpoint"]["identity"]["provider_id"] =
                        serde_json::json!("x".repeat(512));
                }),
            ),
            (
                "identity-path-binding-partial",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["checkpoint"]["identity"]["path_binding"] =
                        serde_json::json!("partial-binding");
                }),
            ),
            (
                "identity-path-binding-replaced",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["checkpoint"]["identity"]["path_binding"] =
                        serde_json::json!("f".repeat(64));
                }),
            ),
            (
                "identity-key-epoch-replaced",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["checkpoint"]["identity"]["key_epoch"] = serde_json::json!(2);
                }),
            ),
            (
                "schema-fence-phase-missing",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["schema_fence"]
                        .as_object_mut()
                        .unwrap()
                        .remove("phase");
                }),
            ),
            (
                "schema-fence-phase-null",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["schema_fence"]["phase"] = serde_json::Value::Null;
                }),
            ),
            (
                "schema-fence-phase-unknown",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["schema_fence"]["phase"] = serde_json::json!("active_v16");
                }),
            ),
            (
                "schema-fence-phase-number",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["schema_fence"]["phase"] = serde_json::json!(15);
                }),
            ),
            (
                "schema-fence-extra-field",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["schema_fence"]["unexpected"] = serde_json::json!(true);
                }),
            ),
            (
                "unknown-root-field",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["unexpected"] = serde_json::json!(true);
                }),
            ),
            (
                "unknown-identity-field",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["checkpoint"]["identity"]["unexpected"] = serde_json::json!(true);
                }),
            ),
            (
                "nested-format-version-marker",
                mutate_system_anchor_payload(valid_payload, |payload| {
                    payload["checkpoint"]["format_version"] = serde_json::json!(2);
                }),
            ),
            (
                "duplicate-provider-id",
                raw_valid_payload.replacen(
                    "\"provider_id\":\"system-keyring\"",
                    "\"provider_id\":\"system-keyring\",\"provider_id\":\"evil\"",
                    1,
                ),
            ),
            (
                "duplicate-format-version",
                raw_valid_payload.replacen(
                    "\"format_version\":3",
                    "\"format_version\":3,\"format_version\":2",
                    1,
                ),
            ),
            (
                "corrupt-json",
                raw_valid_payload.trim_end_matches('}').to_string(),
            ),
        ]
    }

    #[cfg(feature = "system-keyring")]
    async fn read_integrity_provider_binding_state(path: &Path) -> (String, Vec<Option<String>>) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let metadata_provider_id = sqlx::query_scalar::<_, String>(
            "SELECT provider_id FROM integrity_metadata WHERE singleton = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let commit_provider_ids = sqlx::query_scalar::<_, Option<String>>(
            "SELECT provider_id FROM integrity_commits ORDER BY sequence",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        pool.close().await;
        (metadata_provider_id, commit_provider_ids)
    }

    async fn rewrite_integrity_repository_as_legacy_v13(path: &Path, signer: &dyn IntegritySigner) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        let metadata = load_integrity_metadata(&mut transaction, true)
            .await
            .unwrap()
            .unwrap();
        let external = signer.checkpoint().unwrap().unwrap();
        assert_eq!(metadata.format, IntegrityAnchorFormat::ProviderBoundV2);
        assert!(checkpoint_matches_format(
            IntegrityAnchorFormat::ProviderBoundV2,
            &metadata.checkpoint,
            &external
        ));
        let rows = sqlx::query(
            "SELECT sequence,instance_id,key_epoch,provider_id,parent_root,state_digest,root,commit_mac FROM integrity_commits ORDER BY sequence",
        )
        .fetch_all(&mut *transaction)
        .await
        .unwrap();
        let mut legacy_rows = Vec::new();
        let mut legacy_metadata_mac = None;
        for row in rows {
            let commit = decode_integrity_commit_row(&row, true).unwrap();
            let checkpoint = integrity::AnchorCheckpoint {
                identity: external.identity.clone(),
                sequence: commit.sequence,
                root: commit.root.clone(),
            };
            let legacy_mac = sign_checkpoint_with_format(
                signer,
                IntegrityAnchorFormat::LegacyV1,
                &checkpoint,
                &commit.parent_root,
                &commit.state_digest,
            )
            .unwrap();
            if commit.sequence == external.sequence {
                legacy_metadata_mac = Some(legacy_mac.clone());
            }
            legacy_rows.push((commit, legacy_mac));
        }
        let legacy_metadata_mac = legacy_metadata_mac.unwrap();
        sqlx::query("ALTER TABLE integrity_metadata RENAME TO integrity_metadata_v14_test")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query("ALTER TABLE integrity_commits RENAME TO integrity_commits_v14_test")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE integrity_metadata (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                instance_id TEXT NOT NULL,
                path_binding TEXT NOT NULL CHECK(length(path_binding) = 64),
                key_epoch INTEGER NOT NULL CHECK(key_epoch > 0),
                sequence INTEGER NOT NULL CHECK(sequence >= 0),
                root TEXT NOT NULL CHECK(length(root) = 64),
                state_digest TEXT NOT NULL CHECK(length(state_digest) = 64),
                commit_mac TEXT NOT NULL CHECK(length(commit_mac) = 64),
                status TEXT NOT NULL CHECK(status IN ('active','recovery_required'))
            )"#,
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            r#"CREATE TABLE integrity_commits (
                sequence INTEGER PRIMARY KEY CHECK(sequence >= 0),
                instance_id TEXT NOT NULL,
                key_epoch INTEGER NOT NULL CHECK(key_epoch > 0),
                parent_root TEXT NOT NULL,
                state_digest TEXT NOT NULL CHECK(length(state_digest) = 64),
                root TEXT NOT NULL UNIQUE CHECK(length(root) = 64),
                commit_mac TEXT NOT NULL CHECK(length(commit_mac) = 64)
            )"#,
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO integrity_metadata(singleton,instance_id,path_binding,key_epoch,sequence,root,state_digest,commit_mac,status) VALUES (1,?,?,?,?,?,?,?,'active')",
        )
        .bind(&external.identity.instance_id)
        .bind(&external.identity.path_binding)
        .bind(i64::try_from(external.identity.key_epoch).unwrap())
        .bind(i64::try_from(external.sequence).unwrap())
        .bind(&external.root)
        .bind(&metadata.state_digest)
        .bind(&legacy_metadata_mac)
        .execute(&mut *transaction)
        .await
        .unwrap();
        for (commit, legacy_mac) in legacy_rows {
            sqlx::query(
                "INSERT INTO integrity_commits(sequence,instance_id,key_epoch,parent_root,state_digest,root,commit_mac) VALUES (?,?,?,?,?,?,?)",
            )
            .bind(i64::try_from(commit.sequence).unwrap())
            .bind(&commit.instance_id)
            .bind(i64::try_from(commit.key_epoch).unwrap())
            .bind(&commit.parent_root)
            .bind(&commit.state_digest)
            .bind(&commit.root)
            .bind(&legacy_mac)
            .execute(&mut *transaction)
            .await
            .unwrap();
        }
        sqlx::query("DROP TABLE integrity_metadata_v14_test")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query("DROP TABLE integrity_commits_v14_test")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query("DELETE FROM schema_version WHERE version IN (14, 15)")
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        pool.close().await;
    }

    async fn sqlite_table_has_column(path: &Path, table: &str, column: &str) -> bool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let exists = sqlx::query_scalar::<_, String>(&format!(
            "SELECT name FROM pragma_table_info('{table}') WHERE name = ?"
        ))
        .bind(column)
        .fetch_optional(&pool)
        .await
        .unwrap()
        .is_some();
        pool.close().await;
        exists
    }

    async fn sqlite_schema_object_exists(
        path: &Path,
        object_type: &str,
        object_name: &str,
    ) -> bool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = ? AND name = ?",
        )
        .bind(object_type)
        .bind(object_name)
        .fetch_one(&pool)
        .await
        .unwrap()
            > 0;
        pool.close().await;
        exists
    }

    async fn sqlite_table_columns(path: &Path, table: &str) -> Vec<String> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let columns = sqlx::query(&format!("PRAGMA table_info('{table}')"))
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|row| row.try_get::<String, _>("name").ok())
            .collect();
        pool.close().await;
        columns
    }

    async fn sqlite_schema_versions(path: &Path) -> Vec<i64> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let versions =
            sqlx::query_scalar::<_, i64>("SELECT version FROM schema_version ORDER BY version")
                .fetch_all(&pool)
                .await
                .unwrap();
        pool.close().await;
        versions
    }

    async fn sqlite_integrity_commit_count(path: &Path) -> i64 {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM integrity_commits")
            .fetch_one(&pool)
            .await
            .unwrap();
        pool.close().await;
        count
    }

    #[derive(Clone, Copy, Debug)]
    enum ScopedCheckpointTamperCase {
        MetadataRoot,
        MetadataCommitMac,
        CommitMac,
        ParentRoot,
        TruncateTailCommit,
    }

    impl ScopedCheckpointTamperCase {
        fn name(self) -> &'static str {
            match self {
                Self::MetadataRoot => "metadata_root",
                Self::MetadataCommitMac => "metadata_commit_mac",
                Self::CommitMac => "commit_mac",
                Self::ParentRoot => "parent_root",
                Self::TruncateTailCommit => "truncate_tail_commit",
            }
        }
    }

    struct RecycledCheckpointSeed {
        signer_key: usize,
        path_binding: String,
        metadata_sequence: i64,
        commit_count: i64,
        tail_sequence: i64,
    }

    async fn sqlite_string_scalar(path: &Path, query: &str) -> String {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let value = sqlx::query_scalar::<_, String>(query)
            .fetch_one(&pool)
            .await
            .unwrap();
        pool.close().await;
        value
    }

    async fn sqlite_i64_scalar(path: &Path, query: &str) -> i64 {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let value = sqlx::query_scalar::<_, i64>(query)
            .fetch_one(&pool)
            .await
            .unwrap();
        pool.close().await;
        value
    }

    async fn seed_repository_with_recycled_checkpoint_state(
        path: &Path,
        signer: Arc<dyn IntegritySigner>,
        managed_mcp_id: &str,
        mcp_id: &str,
    ) -> RecycledCheckpointSeed {
        let signer_key = path_bound_test_signer_registry_key(&signer);
        let path_binding = database_path_binding(path).unwrap();

        {
            let repository =
                SqliteMcpPlatformRepository::open_path_with_integrity_signer(path, signer.clone())
                    .await
                    .unwrap();
            assert_eq!(
                repository
                    .projection_writer_repository_identity()
                    .unwrap()
                    .path_binding,
                path_binding
            );
            repository
                .create_managed_mcp(
                    &NewManagedMcp {
                        managed_mcp_id: managed_mcp_id.to_string(),
                        mcp_id: mcp_id.to_string(),
                        installation_scope: "user".to_string(),
                    },
                    1,
                )
                .await
                .unwrap();
            repository.close().await;
            assert_eq!(
                path_bound_test_signer_registry_entry_count_for_testing(signer_key),
                1
            );
        }

        assert_eq!(
            path_bound_test_signer_registry_entry_count_for_testing(signer_key),
            0
        );

        let metadata_sequence = sqlite_i64_scalar(
            path,
            "SELECT sequence FROM integrity_metadata WHERE singleton = 1",
        )
        .await;
        let commit_count = sqlite_integrity_commit_count(path).await;
        let tail_sequence = sqlite_i64_scalar(
            path,
            "SELECT COALESCE(MAX(sequence), -1) FROM integrity_commits",
        )
        .await;
        assert!(commit_count >= 2);
        assert_eq!(metadata_sequence, tail_sequence);

        RecycledCheckpointSeed {
            signer_key,
            path_binding,
            metadata_sequence,
            commit_count,
            tail_sequence,
        }
    }

    #[cfg(feature = "system-keyring")]
    async fn create_system_keyring_s0_state(
        path: &Path,
    ) -> (
        Arc<dyn IntegritySigner>,
        String,
        integrity::AnchorCheckpoint,
    ) {
        let path_binding = database_path_binding(path).unwrap();
        let signer = integrity::system_signer(&path_binding, true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true)
                    .foreign_keys(true)
                    .busy_timeout(Duration::from_secs(30))
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await
            .unwrap();
        let schema_was_absent = migrate_through_version(&pool, 14).await.unwrap();
        initialize_or_verify_integrity(
            &pool,
            signer.as_ref(),
            schema_was_absent,
            Some(&path_binding),
        )
        .await
        .unwrap();
        let checkpoint = verified_external_checkpoint(&pool, signer.as_ref(), Some(&path_binding))
            .await
            .unwrap();
        pool.close().await;
        (signer, path_binding, checkpoint)
    }

    async fn create_v20_repository_fixture(path: &Path, signer: Arc<dyn IntegritySigner>) {
        let path_binding = database_path_binding(path).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true)
                    .foreign_keys(true)
                    .busy_timeout(Duration::from_secs(30))
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await
            .unwrap();
        let schema_was_absent = migrate_through_version(&pool, 20).await.unwrap();
        assert!(schema_was_absent);
        initialize_or_verify_integrity(&pool, signer.as_ref(), true, Some(&path_binding))
            .await
            .unwrap();
        pool.close().await;
    }

    async fn create_v21_repository_fixture(path: &Path, signer: Arc<dyn IntegritySigner>) {
        let path_binding = database_path_binding(path).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true)
                    .foreign_keys(true)
                    .busy_timeout(Duration::from_secs(30))
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await
            .unwrap();
        let schema_was_absent = migrate_through_version(&pool, 21).await.unwrap();
        assert!(schema_was_absent);
        initialize_or_verify_integrity(&pool, signer.as_ref(), true, Some(&path_binding))
            .await
            .unwrap();
        pool.close().await;
    }

    async fn create_repository_fixture_with_version(
        path: &Path,
        signer: Arc<dyn IntegritySigner>,
        version: i64,
    ) {
        let path_binding = database_path_binding(path).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true)
                    .foreign_keys(true)
                    .busy_timeout(Duration::from_secs(30))
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await
            .unwrap();
        let schema_was_absent = migrate_through_version(&pool, version).await.unwrap();
        assert!(schema_was_absent);
        initialize_or_verify_integrity(&pool, signer.as_ref(), true, Some(&path_binding))
            .await
            .unwrap();
        pool.close().await;
    }

    async fn create_schema_fixture_with_version(path: &Path, version: i64) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true)
                    .foreign_keys(true)
                    .busy_timeout(Duration::from_secs(30))
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await
            .unwrap();
        let schema_was_absent = migrate_through_version(&pool, version).await.unwrap();
        assert!(schema_was_absent);
        pool.close().await;
    }

    async fn initialize_integrity_fixture(path: &Path, signer: Arc<dyn IntegritySigner>) {
        let path_binding = database_path_binding(path).unwrap();
        let pool = existing_repository_pool(path).await;
        initialize_or_verify_integrity(&pool, signer.as_ref(), true, Some(&path_binding))
            .await
            .unwrap();
        pool.close().await;
    }

    async fn existing_repository_pool(path: &Path) -> Pool<Sqlite> {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .foreign_keys(true)
                    .busy_timeout(Duration::from_secs(30))
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await
            .unwrap()
    }

    async fn apply_v26_schema_without_version_and_publish_integrity(
        path: &Path,
        signer: Arc<dyn IntegritySigner>,
        post_apply_statements: &[&str],
    ) {
        let path_binding = database_path_binding(path).unwrap();
        let pool = existing_repository_pool(path).await;
        let expected_current =
            verified_external_checkpoint(&pool, signer.as_ref(), Some(&path_binding))
                .await
                .unwrap();
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let verified_current = verify_integrity_transaction(&mut transaction, signer.as_ref())
            .await
            .unwrap();
        assert_eq!(verified_current, expected_current);
        migrations::apply_v26(&mut transaction).await.unwrap();
        for statement in post_apply_statements {
            sqlx::query(statement)
                .execute(&mut *transaction)
                .await
                .unwrap();
        }
        let next = append_integrity_checkpoint_without_external_publish(
            &mut transaction,
            signer.as_ref(),
            &verified_current,
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        signer.publish(Some(&verified_current), &next).unwrap();
        pool.close().await;
    }

    #[cfg(feature = "system-keyring")]
    fn downgrade_system_guard_to_v2(raw_guard: &str) -> String {
        let mut downgraded = serde_json::from_str::<serde_json::Value>(raw_guard).unwrap();
        downgraded.as_object_mut().unwrap().remove("schema_fence");
        downgraded["format_version"] = serde_json::json!(2);
        serde_json::to_string(&downgraded).unwrap()
    }

    async fn sqlite_table_row_count(path: &Path, table: &str) -> i64 {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let count = sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        pool.close().await;
        count
    }

    async fn sqlite_table_indexes(path: &Path, table: &str) -> Vec<String> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let mut indexes = sqlx::query(&format!("PRAGMA index_list('{table}')"))
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|row| row.try_get::<String, _>("name").ok())
            .collect::<Vec<_>>();
        indexes.sort();
        pool.close().await;
        indexes
    }

    async fn mutate_schema_versions(path: &Path, sql: &str, version: i64) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        sqlx::query(sql).bind(version).execute(&pool).await.unwrap();
        pool.close().await;
    }

    async fn execute_sql_batch(path: &Path, statements: &[&str]) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        for statement in statements {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
        pool.close().await;
    }

    async fn execute_sql_batch_and_publish_integrity(
        path: &Path,
        signer: Arc<dyn IntegritySigner>,
        statements: &[&str],
    ) {
        let path_binding = database_path_binding(path).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .foreign_keys(true)
                    .busy_timeout(Duration::from_secs(30))
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await
            .unwrap();
        let expected_current =
            verified_external_checkpoint(&pool, signer.as_ref(), Some(&path_binding))
                .await
                .unwrap();
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let verified_current = verify_integrity_transaction(&mut transaction, signer.as_ref())
            .await
            .unwrap();
        assert_eq!(verified_current, expected_current);
        for statement in statements {
            sqlx::query(statement)
                .execute(&mut *transaction)
                .await
                .unwrap();
        }
        let next = append_integrity_checkpoint_without_external_publish(
            &mut transaction,
            signer.as_ref(),
            &verified_current,
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        signer.publish(Some(&verified_current), &next).unwrap();
        pool.close().await;
    }

    async fn rewrite_manifest_blobs_as_legacy_v26(path: &Path, signer: Arc<dyn IntegritySigner>) {
        execute_sql_batch_and_publish_integrity(
            path,
            signer,
            &[
                "ALTER TABLE manifest_blobs RENAME TO manifest_blobs_v27",
                r#"CREATE TABLE manifest_blobs (
                    manifest_digest TEXT PRIMARY KEY CHECK(length(manifest_digest) = 64),
                    mcp_id TEXT NOT NULL,
                    version TEXT NOT NULL,
                    canonical_bytes BLOB NOT NULL,
                    proof_json TEXT NOT NULL,
                    trust_tier_json TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    UNIQUE(mcp_id, version)
                )"#,
                r#"INSERT INTO manifest_blobs(
                    manifest_digest,mcp_id,version,canonical_bytes,proof_json,trust_tier_json,created_at_ms
                )
                SELECT
                    manifest_digest,mcp_id,version,canonical_bytes,proof_json,trust_tier_json,created_at_ms
                FROM manifest_blobs_v27"#,
                "DROP TABLE manifest_blobs_v27",
                "DELETE FROM schema_version WHERE version = 27",
            ],
        )
        .await;
    }

    async fn mutate_projection_mutations_indexes(path: &Path, statements: &[&str]) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        for statement in statements {
            sqlx::query(statement)
                .execute(&mut *transaction)
                .await
                .unwrap();
        }
        transaction.commit().await.unwrap();
        pool.close().await;
    }

    async fn rewrite_projection_mutations_as_legacy_v14(path: &Path) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        sqlx::query("DROP INDEX projection_mutations_active")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query("ALTER TABLE projection_mutations RENAME TO projection_mutations_test_current")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE projection_mutations (
                mutation_id INTEGER PRIMARY KEY AUTOINCREMENT,
                managed_mcp_id TEXT NOT NULL,
                expected_revision INTEGER NOT NULL,
                previous_enabled INTEGER NOT NULL CHECK(previous_enabled IN (0,1)),
                desired_enabled INTEGER NOT NULL CHECK(desired_enabled IN (0,1)),
                status TEXT NOT NULL CHECK(status IN ('started','config_committed','committed','recovery_required','resolved')),
                writer_runtime_id TEXT,
                writer_sink_id TEXT,
                writer_anchor_instance_id TEXT,
                writer_anchor_path_binding TEXT CHECK(writer_anchor_path_binding IS NULL OR length(writer_anchor_path_binding) = 64),
                writer_anchor_key_epoch INTEGER CHECK(writer_anchor_key_epoch IS NULL OR writer_anchor_key_epoch > 0),
                writer_anchor_sequence INTEGER CHECK(writer_anchor_sequence IS NULL OR writer_anchor_sequence >= 0),
                writer_anchor_root TEXT CHECK(writer_anchor_root IS NULL OR length(writer_anchor_root) = 64),
                writer_commitment TEXT CHECK(writer_commitment IS NULL OR length(writer_commitment) = 64),
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            )"#,
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO projection_mutations(
                mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,
                writer_runtime_id,writer_sink_id,writer_anchor_instance_id,writer_anchor_path_binding,
                writer_anchor_key_epoch,writer_anchor_sequence,writer_anchor_root,writer_commitment,
                created_at_ms,updated_at_ms
            )
            SELECT mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,
                   writer_runtime_id,writer_sink_id,writer_anchor_instance_id,writer_anchor_path_binding,
                   writer_anchor_key_epoch,writer_anchor_sequence,writer_anchor_root,writer_commitment,
                   created_at_ms,updated_at_ms
            FROM projection_mutations_test_current"#,
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query("DROP TABLE projection_mutations_test_current")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query("CREATE UNIQUE INDEX projection_mutations_active ON projection_mutations(managed_mcp_id) WHERE status IN ('started','config_committed','recovery_required')")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query("DELETE FROM schema_version WHERE version = ?")
            .bind(migrations::CURRENT_SCHEMA_VERSION)
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        pool.close().await;
    }

    async fn rewrite_managed_enrollments_as_legacy_v17(path: &Path) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        sqlx::query(
            "ALTER TABLE managed_credential_enrollments RENAME TO managed_credential_enrollments_test_current",
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            r#"CREATE TABLE managed_credential_enrollments (
                managed_mcp_id TEXT PRIMARY KEY REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT,
                manifest_digest TEXT NOT NULL REFERENCES manifest_blobs(manifest_digest) ON DELETE RESTRICT,
                auth_schema_id TEXT NOT NULL,
                credential_reference TEXT NOT NULL,
                reference_digest TEXT NOT NULL CHECK(length(reference_digest) = 64),
                authority_json TEXT NOT NULL,
                revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
                updated_at_ms INTEGER NOT NULL
            )"#,
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO managed_credential_enrollments(
                managed_mcp_id,manifest_digest,auth_schema_id,credential_reference,
                reference_digest,authority_json,revision,updated_at_ms
            )
            SELECT managed_mcp_id,manifest_digest,auth_schema_id,credential_reference,
                   reference_digest,authority_json,revision,updated_at_ms
            FROM managed_credential_enrollments_test_current"#,
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query("DROP TABLE managed_credential_enrollments_test_current")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query("DELETE FROM schema_version WHERE version = ?")
            .bind(migrations::CURRENT_SCHEMA_VERSION)
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        pool.close().await;
    }

    async fn rewrite_projection_mutations_with_current_witness_shape(
        path: &Path,
        create_table_sql: &str,
        recreate_active_index: bool,
    ) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        sqlx::query("DROP INDEX IF EXISTS projection_mutations_active")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query("ALTER TABLE projection_mutations RENAME TO projection_mutations_test_current")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query(create_table_sql)
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO projection_mutations(
                mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,
                writer_runtime_id,writer_sink_id,writer_anchor_instance_id,writer_anchor_path_binding,
                writer_anchor_key_epoch,writer_anchor_sequence,writer_anchor_root,writer_witness_version,
                writer_provider_id,writer_binding_domain,writer_observed_state_digest,writer_commitment,
                created_at_ms,updated_at_ms
            )
            SELECT mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,
                   writer_runtime_id,writer_sink_id,writer_anchor_instance_id,writer_anchor_path_binding,
                   writer_anchor_key_epoch,writer_anchor_sequence,writer_anchor_root,writer_witness_version,
                   writer_provider_id,writer_binding_domain,writer_observed_state_digest,writer_commitment,
                   created_at_ms,updated_at_ms
            FROM projection_mutations_test_current"#,
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query("DROP TABLE projection_mutations_test_current")
            .execute(&mut *transaction)
            .await
            .unwrap();
        if recreate_active_index {
            sqlx::query("CREATE UNIQUE INDEX projection_mutations_active ON projection_mutations(managed_mcp_id) WHERE status IN ('started','config_committed','recovery_required')")
                .execute(&mut *transaction)
                .await
                .unwrap();
        }
        sqlx::query("DELETE FROM schema_version WHERE version = ?")
            .bind(migrations::CURRENT_SCHEMA_VERSION)
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        pool.close().await;
    }

    async fn rewrite_projection_mutations_with_partial_witness_v2_column(path: &Path) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        sqlx::query("DROP INDEX projection_mutations_active")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query("ALTER TABLE projection_mutations RENAME TO projection_mutations_test_current")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE projection_mutations (
                mutation_id INTEGER PRIMARY KEY AUTOINCREMENT,
                managed_mcp_id TEXT NOT NULL,
                expected_revision INTEGER NOT NULL,
                previous_enabled INTEGER NOT NULL CHECK(previous_enabled IN (0,1)),
                desired_enabled INTEGER NOT NULL CHECK(desired_enabled IN (0,1)),
                status TEXT NOT NULL CHECK(status IN ('started','config_committed','committed','recovery_required','resolved')),
                writer_runtime_id TEXT,
                writer_sink_id TEXT,
                writer_anchor_instance_id TEXT,
                writer_anchor_path_binding TEXT CHECK(writer_anchor_path_binding IS NULL OR length(writer_anchor_path_binding) = 64),
                writer_anchor_key_epoch INTEGER CHECK(writer_anchor_key_epoch IS NULL OR writer_anchor_key_epoch > 0),
                writer_anchor_sequence INTEGER CHECK(writer_anchor_sequence IS NULL OR writer_anchor_sequence >= 0),
                writer_anchor_root TEXT CHECK(writer_anchor_root IS NULL OR length(writer_anchor_root) = 64),
                writer_witness_version INTEGER CHECK(writer_witness_version IS NULL OR writer_witness_version = 2),
                writer_commitment TEXT CHECK(writer_commitment IS NULL OR length(writer_commitment) = 64),
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            )"#,
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO projection_mutations(
                mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,
                writer_runtime_id,writer_sink_id,writer_anchor_instance_id,writer_anchor_path_binding,
                writer_anchor_key_epoch,writer_anchor_sequence,writer_anchor_root,writer_witness_version,
                writer_commitment,created_at_ms,updated_at_ms
            )
            SELECT mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,
                   writer_runtime_id,writer_sink_id,writer_anchor_instance_id,writer_anchor_path_binding,
                   writer_anchor_key_epoch,writer_anchor_sequence,writer_anchor_root,NULL,
                   writer_commitment,created_at_ms,updated_at_ms
            FROM projection_mutations_test_current"#,
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query("DROP TABLE projection_mutations_test_current")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query("CREATE UNIQUE INDEX projection_mutations_active ON projection_mutations(managed_mcp_id) WHERE status IN ('started','config_committed','recovery_required')")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query("DELETE FROM schema_version WHERE version = ?")
            .bind(migrations::CURRENT_SCHEMA_VERSION)
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        pool.close().await;
    }

    struct VersionedProjectionSink(&'static str);

    #[async_trait]
    impl ProjectionSink for VersionedProjectionSink {
        fn adapter_id(&self) -> &'static str {
            "extension_config_sink"
        }

        fn adapter_version(&self) -> &'static str {
            self.0
        }

        async fn put_disabled(
            &self,
            _key: &str,
            _config: crate::agents::ExtensionConfig,
        ) -> McpPlatformResult<ProjectionSnapshot> {
            Err(integrity_error())
        }

        async fn get(&self, _key: &str) -> McpPlatformResult<Option<ProjectionSnapshot>> {
            Err(integrity_error())
        }

        async fn set_enabled(
            &self,
            _key: &str,
            _enabled: bool,
        ) -> McpPlatformResult<ProjectionSnapshot> {
            Err(integrity_error())
        }

        async fn commit_enabled_projection(
            &self,
            _plan: &ProjectionCommitPlan,
        ) -> McpPlatformResult<ProjectionSinkAtomicProof> {
            Err(integrity_error())
        }

        async fn remove_owned(
            &self,
            _key: &str,
            _expected: &ProjectionSnapshot,
        ) -> McpPlatformResult<bool> {
            Err(integrity_error())
        }
    }

    fn test_ports_with_projection_adapter_version(version: &'static str) -> LifecyclePorts {
        LifecyclePorts {
            registration: Arc::new(SafeRegistrationEffectAdapter::default()),
            host_integration: Arc::new(EmptyHostIntegrationAdapter),
            transport: Arc::new(CoreTransportProjectionAdapter),
            auth: Arc::new(ConfigAuthRequirementResolver),
            health: Arc::new(ProductionHealthCheckAdapter::default()),
            projection_sink: Arc::new(VersionedProjectionSink(version)),
        }
    }

    fn test_projection_receipt(
        authority: &crate::mcp_platform::projection_runtime::RuntimeProjectionAuthority,
        authorization: &ProjectionAuthorization,
        observed_state_digest: String,
    ) -> ProjectionSinkCommitReceipt {
        let witness = authorization.witness_v2().unwrap();
        let proof = authority
            .test_only_issue_atomic_proof(
                &witness,
                ProjectionSinkAtomicProofKind::NoopCompareAndSwap,
                observed_state_digest.clone(),
                observed_state_digest,
            )
            .unwrap();
        authority
            .issue_sink_commit_receipt(authorization, proof)
            .unwrap()
    }

    fn forged_projection_proof(
        authorization: &ProjectionAuthorization,
        observed_state_digest: String,
    ) -> ProjectionSinkAtomicProof {
        ProjectionSinkAtomicProof::new(
            "extension_config_sink".to_string(),
            "1".to_string(),
            authorization.witness_v2().unwrap().runtime_id,
            observed_state_digest.clone(),
            observed_state_digest,
            ProjectionSinkAtomicProofKind::NoopCompareAndSwap,
            "0".repeat(64),
        )
        .unwrap()
    }

    async fn save_manifest_and_plan(
        repository: &SqliteMcpPlatformRepository,
        fixture: &str,
        trust_tier: TrustTier,
        plan_id: &str,
        idempotency_key: &str,
    ) -> crate::mcp_platform::InstallationPlan {
        save_manifest_and_plan_with_source_metadata(
            repository,
            fixture,
            trust_tier,
            plan_id,
            idempotency_key,
            ManifestSourceMetadata::local_persistence(),
        )
        .await
    }

    async fn save_manifest_and_plan_with_source_metadata(
        repository: &SqliteMcpPlatformRepository,
        fixture: &str,
        trust_tier: TrustTier,
        plan_id: &str,
        idempotency_key: &str,
        source_metadata: ManifestSourceMetadata,
    ) -> crate::mcp_platform::InstallationPlan {
        let (manifest, plan) = plan(fixture, trust_tier);
        repository
            .save_manifest(&ManifestRecord {
                verified: manifest,
                proof: ManifestProof::LocalBytes,
                trust_tier,
                source_metadata,
                created_at_ms: 10,
            })
            .await
            .unwrap();
        repository
            .save_plan(SavePlan {
                plan_id,
                idempotency_key,
                plan: &plan,
                target: &PlanTarget {
                    managed_mcp_id: None,
                    mcp_id: plan.manifest_id().to_string(),
                    version: plan.manifest_version().to_string(),
                    installation_scope: None,
                    source_context: None,
                },
                policy_evidence: plan.policy(),
                confirmation_evidence: &ConfirmationEvidence::Pending,
                expires_at_ms: 10_000,
                created_at_ms: 20,
                actor: "tester",
            })
            .await
            .unwrap();
        plan
    }

    async fn save_managed_manifest_and_plan_with_source_metadata(
        repository: &SqliteMcpPlatformRepository,
        fixture: &str,
        trust_tier: TrustTier,
        plan_id: &str,
        idempotency_key: &str,
        installation_scope: &str,
        source_metadata: ManifestSourceMetadata,
    ) -> crate::mcp_platform::InstallationPlan {
        let manifest = parse_manifest(fixture.as_bytes()).unwrap();
        let context = PolicyContext::new(trust_tier, PlanOperation::Update)
            .with_target(
                crate::mcp_platform::manifest::Platform::Windows,
                crate::mcp_platform::manifest::Architecture::X86_64,
            )
            .with_runtime_capabilities(true, Some((3, 11)));
        let plan = plan_for_manifest(&manifest, &context).unwrap();
        let managed_mcp_id = crate::mcp_platform::task_runner::stable_managed_mcp_id(
            plan.manifest_id(),
            installation_scope,
        );
        repository
            .save_manifest(&ManifestRecord {
                verified: manifest,
                proof: ManifestProof::LocalBytes,
                trust_tier,
                source_metadata,
                created_at_ms: 10,
            })
            .await
            .unwrap();
        repository
            .save_plan(SavePlan {
                plan_id,
                idempotency_key,
                plan: &plan,
                target: &PlanTarget {
                    managed_mcp_id: Some(managed_mcp_id),
                    mcp_id: plan.manifest_id().to_string(),
                    version: plan.manifest_version().to_string(),
                    installation_scope: Some(installation_scope.to_string()),
                    source_context: None,
                },
                policy_evidence: plan.policy(),
                confirmation_evidence: &ConfirmationEvidence::Pending,
                expires_at_ms: 10_000,
                created_at_ms: 20,
                actor: "tester",
            })
            .await
            .unwrap();
        plan
    }

    fn v27_test_source_metadata() -> ManifestSourceMetadata {
        ManifestSourceMetadata {
            source_ref: SourceRef::VerifiedSourceCatalog {
                source_id: "catalog-source".to_string(),
            },
            import_kind: SourceImportKind::VerifiedSourceCatalog,
            release_id: Some("release-2026-07".to_string()),
            origin_provenance: OriginProvenance {
                verified_source_document: Some(VerifiedSourceDocumentRef {
                    source_id: "catalog-source".to_string(),
                    document_digest: "1".repeat(64),
                    canonical_digest: "2".repeat(64),
                    signed_digest: "3".repeat(64),
                    binding_digest: "4".repeat(64),
                    signature_kids: vec!["kid-1".to_string(), "kid-2".to_string()],
                }),
            },
            update_channel: UpdateChannel::Named {
                name: "stable".to_string(),
            },
        }
    }

    fn governed_catalog_test_import() -> (SaveGovernedCatalogImport, String, String) {
        const ED25519_SPKI_PREFIX: [u8; 12] = [
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];

        let manifest = parse_manifest(REMOTE.as_bytes()).unwrap();
        let source_document_id = "catalog-integrity-source";
        let source_id = format!("verified_source_catalog_{source_document_id}");
        let signing_key = SigningKey::from_bytes(&[0x91; 32]);
        let mut spki_der = ED25519_SPKI_PREFIX.to_vec();
        spki_der.extend_from_slice(&signing_key.verifying_key().to_bytes());
        let root_key = crate::verified_source_catalog::SourceRootKeyV1 {
            kid: format!(
                "ed25519-spki-sha256:{}",
                crate::utils::bytes_to_hex(Sha256::digest(&spki_der))
            ),
            spki_der_b64u: URL_SAFE_NO_PAD.encode(spki_der),
        };
        let payload = crate::verified_source_catalog::SourceUnsignedPayloadV1 {
            schema_version: 1,
            source_id: source_document_id.to_string(),
            source_name: "Catalog integrity source".to_string(),
            issued_at_ms: 1,
            root: crate::verified_source_catalog::SourceTrustRootV1 {
                quorum: 1,
                keys: vec![root_key.clone()],
                previous: None,
            },
            snapshot: crate::verified_source_catalog::SourceSnapshotV1 {
                releases: vec![crate::verified_source_catalog::SourceReleaseV1 {
                    release_id: "release-1".to_string(),
                    mcp_id: manifest.manifest().id.clone(),
                    version: manifest.manifest().version.as_str().to_owned(),
                    manifest_kind: "manifest".to_string(),
                    platform: "windows".to_string(),
                    architecture: "x86_64".to_string(),
                    variant: "msvc".to_string(),
                }],
                revocations: Vec::new(),
            },
        };
        let signature = signing_key.sign(&payload.to_canonical_json_bytes());
        let document = crate::verified_source_catalog::parse_signed_envelope(
            &crate::verified_source_catalog::SourceSignedEnvelopeV1 {
                payload,
                signatures: vec![crate::verified_source_catalog::SourceSignatureV1 {
                    kid: root_key.kid.clone(),
                    sig_b64u: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
                }],
            }
            .to_canonical_json_bytes(),
        )
        .unwrap();
        let source_metadata = ManifestSourceMetadata {
            source_ref: SourceRef::VerifiedSourceCatalog {
                source_id: source_document_id.to_string(),
            },
            import_kind: SourceImportKind::VerifiedSourceCatalog,
            release_id: Some("release-1".to_string()),
            origin_provenance: OriginProvenance {
                verified_source_document: Some(VerifiedSourceDocumentRef {
                    source_id: source_document_id.to_string(),
                    document_digest: document.digests.document_digest.clone(),
                    canonical_digest: document.digests.canonical_digest.clone(),
                    signed_digest: document.digests.signed_digest.clone(),
                    binding_digest: document.digests.binding_digest.clone(),
                    signature_kids: vec![root_key.kid],
                }),
            },
            update_channel: UpdateChannel::Named {
                name: "stable".to_string(),
            },
        };
        let digest = manifest.digest().to_string();
        (
            SaveGovernedCatalogImport {
                source: GovernedCatalogSourceRecord {
                    source_id: source_id.clone(),
                    import_kind: SourceImportKind::VerifiedSourceCatalog,
                    source_ref: source_metadata.source_ref.clone(),
                    display_name: "Catalog integrity source".to_string(),
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
                document: GovernedCatalogDocumentRecord {
                    source_id: source_id.clone(),
                    document_id: "directory-1".to_string(),
                    document_digest: document.digests.document_digest.clone(),
                    document_kind: GovernedCatalogDocumentKind::Directory,
                    created_at_ms: 1,
                },
                verified_source_document: Some(document.clone()),
                entries: vec![SaveGovernedCatalogEntry {
                    entry_id: "release-1".to_string(),
                    manifest: ManifestRecord {
                        verified: manifest,
                        proof: ManifestProof::Catalog {
                            index_digest: document.digests.document_digest,
                            declared_manifest_digest: digest.clone(),
                            signature: None,
                        },
                        trust_tier: TrustTier::Official,
                        source_metadata,
                        created_at_ms: 1,
                    },
                }],
            },
            source_id,
            digest,
        )
    }

    #[tokio::test]
    async fn governed_catalog_projection_reads_verified_import_and_fails_closed_when_missing() {
        let (_directory, repository) =
            open_temp_repository_with_test_signer("catalog-projection.db", [0x92; 32]).await;
        let (input, source_id, digest) = governed_catalog_test_import();
        repository
            .save_governed_catalog_import(input)
            .await
            .unwrap();

        let by_digest = repository.get_manifest(&digest).await.unwrap();
        let with_context = repository
            .get_manifest_with_source_context(
                &digest,
                &ManifestSourceContext::GovernedCatalog {
                    source_id: source_id.clone(),
                },
            )
            .await
            .unwrap();
        let entry = repository
            .get_governed_catalog_entry_by_digest(&digest)
            .await
            .unwrap();
        let mcp_id = entry.manifest.verified.manifest().id.clone();
        let version = entry
            .manifest
            .verified
            .manifest()
            .version
            .as_str()
            .to_owned();
        assert_eq!(by_digest.source_metadata, entry.manifest.source_metadata);
        assert_eq!(with_context.source_metadata, entry.manifest.source_metadata);
        assert_eq!(
            by_digest.source_metadata.release_id.as_deref(),
            Some("release-1")
        );
        assert_eq!(
            by_digest
                .source_metadata
                .origin_provenance
                .verified_source_document
                .as_ref()
                .map(|provenance| provenance.source_id.as_str()),
            Some("catalog-integrity-source")
        );

        sqlx::query("UPDATE source_catalog_documents SET raw_bytes = X'00' WHERE source_id = ?")
            .bind("catalog-integrity-source")
            .execute(&repository.pool)
            .await
            .unwrap();
        assert_eq!(
            repository
                .list_governed_catalog_entries()
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );
        assert_eq!(
            repository
                .list_governed_catalog_sources()
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );
        assert_eq!(
            repository
                .get_governed_catalog_source(&source_id)
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );
        assert_eq!(
            repository
                .source_catalog_document_provenance("catalog-integrity-source")
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );
        assert_eq!(
            repository
                .source_catalog_release_matches(
                    "catalog-integrity-source",
                    "release-1",
                    &mcp_id,
                    &version,
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );

        sqlx::query("DELETE FROM source_catalog_documents WHERE source_id = ?")
            .bind("catalog-integrity-source")
            .execute(&repository.pool)
            .await
            .unwrap();
        assert_eq!(
            repository.get_manifest(&digest).await.unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError
        );
        assert_eq!(
            repository
                .get_manifest_with_source_context(
                    &digest,
                    &ManifestSourceContext::GovernedCatalog { source_id },
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );
    }

    #[tokio::test]
    async fn governed_catalog_projection_rejects_entry_blob_identity_mismatch() {
        let (_directory, repository) =
            open_temp_repository_with_test_signer("catalog-projection-mismatch.db", [0x93; 32])
                .await;
        let (input, source_id, digest) = governed_catalog_test_import();
        repository
            .save_governed_catalog_import(input)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE governed_catalog_entries SET mcp_id = 'other.mcp' WHERE source_id = ? AND manifest_digest = ?",
        )
        .bind(&source_id)
        .bind(&digest)
        .execute(&repository.pool)
        .await
        .unwrap();

        assert_eq!(
            repository
                .get_governed_catalog_entry_by_digest(&digest)
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );
    }

    async fn open_temp_repository_with_test_signer(
        file_name: &str,
        key: [u8; 32],
    ) -> (tempfile::TempDir, SqliteMcpPlatformRepository) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(file_name);
        let repository = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            InMemoryIntegritySigner::new_for_testing(key),
        )
        .await
        .unwrap();
        (directory, repository)
    }

    async fn stage_active_owned_projection(
        repository: &SqliteMcpPlatformRepository,
        plan: &crate::mcp_platform::InstallationPlan,
        plan_id: &str,
        task_id: &str,
        task_key: &str,
        worker_owner_id: &str,
        now_ms: i64,
    ) -> (TaskRecord, String, ConnectionProjectionRecord) {
        let task = create_task(repository, plan_id, plan, task_id, task_key).await;
        let running = advance_to_running(repository, task, now_ms).await;
        let verifying = transition(repository, &running, TaskStatus::Verifying, now_ms + 1).await;
        let activating =
            transition(repository, &verifying, TaskStatus::Activating, now_ms + 2).await;
        let managed_mcp_id =
            crate::mcp_platform::task_runner::stable_managed_mcp_id(plan.manifest_id(), "user");
        let adapter_evidence = test_adapter_evidence();
        let verification_evidence =
            test_verification_evidence(&format!("{:064x}", now_ms), now_ms + 1);
        let materialized_tree_digest = format!("{:064x}", now_ms + 2);
        let owned_relative_paths: Vec<String> = Vec::new();
        repository
            .stage_managed_installation(StageManagedInstallation {
                managed_mcp_id: &managed_mcp_id,
                mcp_id: plan.manifest_id(),
                installation_scope: "user",
                distribution_adapter: &plan.adapter().id,
                manifest_digest: plan.manifest_digest(),
                version: plan.manifest_version(),
                installation_root: "/tmp/managed",
                task_id,
                adapter_evidence: &adapter_evidence,
                verification_evidence: &verification_evidence,
                materialized_tree_digest: &materialized_tree_digest,
                supply_chain_evidence: None,
                owned_relative_paths: &owned_relative_paths,
                now_ms: now_ms + 4,
            })
            .await
            .unwrap();
        repository
            .mark_managed_runtime_activated(
                &managed_mcp_id,
                plan.manifest_version(),
                task_id,
                now_ms + 5,
            )
            .await
            .unwrap();
        repository
            .activate_managed_installation(ActivateManagedInstallation {
                managed_mcp_id: &managed_mcp_id,
                target_version: plan.manifest_version(),
                task_id,
                now_ms: now_ms + 6,
            })
            .await
            .unwrap();
        repository
            .add_task_step_with_adapter(
                task_id,
                8,
                &format!("{task_id}:projection-writer"),
                &CompensationDescriptor::NoCompensation,
                "connection_projection_repository",
                "1",
                worker_owner_id,
                activating.revision,
                now_ms + 7,
            )
            .await
            .unwrap();
        repository
            .transition_task_step(StepTransition {
                task_id,
                ordinal: 8,
                owner_id: worker_owner_id,
                expected_task_revision: activating.revision,
                expected_status: TaskStepStatus::NotStarted,
                next_status: TaskStepStatus::Started,
                evidence: None,
                actor: worker_owner_id,
                now_ms: now_ms + 8,
            })
            .await
            .unwrap();
        let projection = repository
            .put_owned_connection_projection(PutOwnedProjection {
                plan_id,
                owner_task_id: task_id,
                worker_owner_id,
                step_ordinal: 8,
                step_token: &format!("{task_id}:projection-writer"),
                now_ms: now_ms + 9,
            })
            .await
            .unwrap();
        let task = repository.get_task(task_id).await.unwrap();
        repository
            .transition_task_step(StepTransition {
                task_id,
                ordinal: 8,
                owner_id: worker_owner_id,
                expected_task_revision: task.revision,
                expected_status: TaskStepStatus::Started,
                next_status: TaskStepStatus::Committed,
                evidence: Some(&StepEvidence::ProjectionPersisted {
                    managed_mcp_id: managed_mcp_id.clone(),
                    link_key: projection.link_key.clone(),
                    revision: projection.revision,
                    created: projection.owner_task_id.as_deref() == Some(task_id),
                }),
                actor: worker_owner_id,
                now_ms: now_ms + 10,
            })
            .await
            .unwrap();
        let task = repository.get_task(task_id).await.unwrap();
        let task = transition(repository, &task, TaskStatus::Succeeded, now_ms + 11).await;
        (task, managed_mcp_id, projection)
    }

    async fn create_task(
        repository: &SqliteMcpPlatformRepository,
        plan_id: &str,
        plan: &crate::mcp_platform::InstallationPlan,
        task_id: &str,
        key: &str,
    ) -> TaskRecord {
        repository
            .create_task(CreateTask {
                task_id,
                plan_id,
                plan_digest: plan.plan_digest(),
                operation: crate::mcp_platform::task::TaskOperation::from(plan.operation()),
                idempotency_key: key,
                actor: "tester",
                now_ms: 30,
                adapter_evidence: None,
                rollback_evidence: None,
            })
            .await
            .unwrap()
    }

    async fn transition(
        repository: &SqliteMcpPlatformRepository,
        task: &TaskRecord,
        next_status: TaskStatus,
        now_ms: i64,
    ) -> TaskRecord {
        repository
            .transition_task(TaskTransition {
                task_id: &task.task_id,
                expected_revision: task.revision,
                next_status,
                actor: "tester",
                now_ms,
                heartbeat_at_ms: Some(now_ms),
                progress: task.progress,
                redacted_error: task.redacted_error.as_ref(),
                rollback_status: task.rollback_status,
                rollback_evidence: task.rollback_evidence.as_ref(),
            })
            .await
            .unwrap()
    }

    async fn advance_to_running(
        repository: &SqliteMcpPlatformRepository,
        mut task: TaskRecord,
        now_ms: i64,
    ) -> TaskRecord {
        task = transition(repository, &task, TaskStatus::AwaitingConfirmation, now_ms).await;
        task = repository
            .confirm_task(&task.task_id, task.revision, "tester", now_ms + 1)
            .await
            .unwrap();
        repository
            .claim_next_task("tester", now_ms + 2, 30_000)
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn claim_next_task_cancels_expired_queued_task_and_claims_following_task() {
        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            InMemoryIntegritySigner::new_for_testing([0x5e; 32]),
        )
        .await
        .unwrap();
        let expired_plan = save_manifest_and_plan(
            &repository,
            REMOTE,
            TrustTier::Official,
            "expired-plan",
            "expired-key",
        )
        .await;
        let (_, valid_plan) = plan(REMOTE, TrustTier::Official);
        repository
            .save_plan(SavePlan {
                plan_id: "valid-plan",
                idempotency_key: "valid-key",
                plan: &valid_plan,
                target: &PlanTarget {
                    managed_mcp_id: None,
                    mcp_id: valid_plan.manifest_id().to_string(),
                    version: valid_plan.manifest_version().to_string(),
                    installation_scope: Some("system".to_string()),
                    source_context: None,
                },
                policy_evidence: valid_plan.policy(),
                confirmation_evidence: &ConfirmationEvidence::Pending,
                expires_at_ms: profile_lifecycle_test_expiry_ms(),
                created_at_ms: 20,
                actor: "tester",
            })
            .await
            .unwrap();

        let expired = create_task(
            &repository,
            "expired-plan",
            &expired_plan,
            "expired-task",
            "expired-task-key",
        )
        .await;
        let expired = transition(&repository, &expired, TaskStatus::AwaitingConfirmation, 40).await;
        let expired = repository
            .confirm_task(&expired.task_id, expired.revision, "tester", 41)
            .await
            .unwrap();
        let valid = create_task(
            &repository,
            "valid-plan",
            &valid_plan,
            "valid-task",
            "valid-task-key",
        )
        .await;
        let valid = transition(&repository, &valid, TaskStatus::AwaitingConfirmation, 42).await;
        repository
            .confirm_task(&valid.task_id, valid.revision, "tester", 43)
            .await
            .unwrap();
        let mut transaction = repository.begin_immediate().await.unwrap();
        sqlx::query("UPDATE tasks SET owner_id = ?, lease_expires_at_ms = ? WHERE task_id = ?")
            .bind("stale-worker")
            .bind(9_999)
            .bind(&expired.task_id)
            .execute(&mut **transaction)
            .await
            .unwrap();
        sqlx::query("UPDATE managed_lifecycle_leases SET expires_at_ms = ? WHERE task_id = ?")
            .bind(9_999)
            .bind(&expired.task_id)
            .execute(&mut **transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let (first_claim, second_claim) = tokio::join!(
            repository.claim_next_task("worker-a", 10_000, 30_000),
            repository.claim_next_task("worker-b", 10_000, 30_000),
        );
        let claims = [first_claim.unwrap(), second_claim.unwrap()];
        assert_eq!(claims.iter().filter(|claim| claim.is_some()).count(), 1);
        let claimed = claims.into_iter().flatten().next().unwrap();

        assert_eq!(claimed.task_id, "valid-task");
        assert_eq!(claimed.status, TaskStatus::Running);
        let valid_lifecycle_target = sqlx::query_scalar::<_, String>(
            "SELECT managed_mcp_id FROM managed_lifecycle_leases WHERE task_id = ?",
        )
        .bind(&claimed.task_id)
        .fetch_one(&repository.pool)
        .await
        .unwrap();
        assert_eq!(
            valid_lifecycle_target,
            crate::mcp_platform::task_runner::stable_managed_mcp_id(
                valid_plan.manifest_id(),
                "system"
            )
        );
        let expired = repository.get_task("expired-task").await.unwrap();
        assert_eq!(expired.status, TaskStatus::Cancelled);
        assert_eq!(expired.owner_id, None);
        assert_eq!(expired.lease_expires_at_ms, None);
        assert!(expired.redacted_error.as_ref().is_some_and(|error| {
            error.code() == RedactedErrorCode::VerificationFailed
                && error.message() == "MCP task plan expired before execution authorization"
        }));
        let lifecycle_lease_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM managed_lifecycle_leases WHERE task_id = ?",
        )
        .bind("expired-task")
        .fetch_one(&repository.pool)
        .await
        .unwrap();
        assert_eq!(lifecycle_lease_count, 0);
        let events = repository.list_audit_events("expired-task").await.unwrap();
        assert!(events.iter().any(|event| {
            event.event_type == AuditEventType::TaskStatusChanged
                && event.payload
                    == AuditPayload::TaskStatusChanged {
                        from: TaskStatus::Queued,
                        to: TaskStatus::Cancelled,
                    }
                && event.redacted_error.as_ref().is_some_and(|error| {
                    error.code() == RedactedErrorCode::VerificationFailed
                        && error.message() == "MCP task plan expired before execution authorization"
                })
        }));
    }

    fn test_adapter_evidence() -> crate::mcp_platform::AdapterEvidence {
        crate::mcp_platform::AdapterEvidence {
            adapter_id: "adapter.test".to_string(),
            adapter_version: "1".to_string(),
            compatible_for_recovery: true,
            resume_safe: true,
        }
    }

    fn test_verification_evidence(
        artifact_digest: &str,
        installed_at_ms: i64,
    ) -> crate::mcp_platform::managed_distribution::ArtifactVerificationEvidence {
        crate::mcp_platform::managed_distribution::ArtifactVerificationEvidence {
            source_origin: "https://example.test/package.tgz".to_string(),
            artifact_digest: artifact_digest.to_string(),
            size_bytes: 42,
            adapter_id: "adapter.test".to_string(),
            adapter_version: "1".to_string(),
            platform_selector: "linux-x86_64".to_string(),
            verification_result:
                crate::mcp_platform::managed_distribution::VerificationResult::Verified,
            installed_at_ms,
            artifact_signature:
                crate::mcp_platform::managed_distribution::ArtifactSignatureStatus::NotDeclaredByManifestV1,
        }
    }

    async fn seed_pointer_committed_activation(
        repository: &SqliteMcpPlatformRepository,
        managed_mcp_id: &str,
        task_id: &str,
        now_ms: i64,
    ) -> String {
        let plan_id = format!("{managed_mcp_id}-plan");
        let key = format!("{managed_mcp_id}-key");
        let plan =
            save_manifest_and_plan(repository, REMOTE, TrustTier::Official, &plan_id, &key).await;
        let task = create_task(repository, &plan_id, &plan, task_id, &key).await;
        let adapter_evidence = test_adapter_evidence();
        let verification_evidence = test_verification_evidence(&format!("{:064x}", now_ms), now_ms);
        let materialized_tree_digest = format!("{:064x}", now_ms + 1);
        let owned_relative_paths: Vec<String> = Vec::new();
        sqlx::query(
            "INSERT INTO managed_lifecycle_leases(managed_mcp_id,task_id,operation,acquired_at_ms,expires_at_ms) VALUES (?,?,'update',?,?)",
        )
        .bind(managed_mcp_id)
        .bind(&task.task_id)
        .bind(now_ms)
        .bind(now_ms + 60_000)
        .execute(&repository.pool)
        .await
        .unwrap();
        repository
            .stage_managed_installation(crate::mcp_platform::repository::StageManagedInstallation {
                managed_mcp_id,
                mcp_id: plan.manifest_id(),
                installation_scope: "user",
                distribution_adapter: "adapter.test",
                manifest_digest: plan.manifest_digest(),
                version: plan.manifest_version(),
                installation_root: "/tmp/managed",
                task_id: &task.task_id,
                adapter_evidence: &adapter_evidence,
                verification_evidence: &verification_evidence,
                materialized_tree_digest: &materialized_tree_digest,
                supply_chain_evidence: None,
                owned_relative_paths: &owned_relative_paths,
                now_ms,
            })
            .await
            .unwrap();
        repository
            .mark_managed_runtime_activated(
                managed_mcp_id,
                plan.manifest_version(),
                &task.task_id,
                now_ms + 1,
            )
            .await
            .unwrap();
        plan.manifest_version().to_string()
    }

    #[tokio::test]
    async fn activation_preserves_authorized_default_enabled_and_never_bootstraps_true() {
        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            InMemoryIntegritySigner::new_for_testing([0x96; 32]),
        )
        .await
        .unwrap();

        let fresh_version = seed_pointer_committed_activation(
            &repository,
            "activation-fresh",
            "activation-fresh-task",
            100,
        )
        .await;
        let fresh = repository
            .activate_managed_installation(
                crate::mcp_platform::repository::ActivateManagedInstallation {
                    managed_mcp_id: "activation-fresh",
                    target_version: &fresh_version,
                    task_id: "activation-fresh-task",
                    now_ms: 102,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            fresh.managed.state.installation,
            crate::mcp_platform::InstallationState::Installed
        );
        assert!(!fresh.managed.state.default_enabled);

        let preserved_version = seed_pointer_committed_activation(
            &repository,
            "activation-preserved",
            "activation-preserved-task",
            200,
        )
        .await;
        let state_json = sqlx::query_scalar::<_, String>(
            "SELECT state_json FROM managed_mcps WHERE managed_mcp_id = ?",
        )
        .bind("activation-preserved")
        .fetch_one(&repository.pool)
        .await
        .unwrap();
        let mut preserved_state = decode::<ManagedMcpState>(&state_json).unwrap();
        preserved_state.default_enabled = true;
        sqlx::query(
            "UPDATE managed_mcps SET state_json = ?, updated_at_ms = ?, revision = revision + 1 WHERE managed_mcp_id = ?",
        )
        .bind(encode(&preserved_state).unwrap())
        .bind(201)
        .bind("activation-preserved")
        .execute(&repository.pool)
        .await
        .unwrap();

        let preserved = repository
            .activate_managed_installation(
                crate::mcp_platform::repository::ActivateManagedInstallation {
                    managed_mcp_id: "activation-preserved",
                    target_version: &preserved_version,
                    task_id: "activation-preserved-task",
                    now_ms: 202,
                },
            )
            .await
            .unwrap();
        assert!(preserved.managed.state.default_enabled);
    }

    #[test]
    fn public_default_enabled_inputs_stay_non_writable() {
        let repository_source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/mcp_platform/repository/mod.rs"
        ));
        let activation_block = repository_source
            .split("pub struct ActivateManagedInstallation<'a> {")
            .nth(1)
            .unwrap()
            .split("}")
            .next()
            .unwrap();
        assert!(!activation_block.contains("default_enabled"));

        let update_block = repository_source
            .split("pub struct ManagedMcpStateUpdate {")
            .nth(1)
            .unwrap()
            .split("}")
            .next()
            .unwrap();
        assert!(!update_block.contains("default_enabled"));

        let port_source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/mcp_platform/service/port.rs"
        ));
        assert!(port_source.contains("state: &ManagedMcpStateUpdate"));
        for forbidden in [
            "begin_projection_mutation",
            "authorize_projection_mutation",
            "mark_projection_config_committed_authorized",
            "complete_projection_mutation_authorized",
        ] {
            assert!(!port_source.contains(forbidden), "{forbidden}");
        }

        let lifecycle_source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/mcp_platform/repository/sqlite/lifecycle_repository.rs"
        ));
        assert!(!lifecycle_source.contains("pub async fn begin_projection_mutation("));
        assert!(
            !lifecycle_source.contains("pub async fn mark_projection_mutation_recovery_required(")
        );

        let runner_source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/mcp_platform/task_runner.rs"
        ));
        assert!(!runner_source.contains("pub(crate) fn new("));
    }

    fn legacy_task_step_binding_payload(
        task: &TaskRecord,
        step: &TaskStepRecord,
        compensation_json: &str,
    ) -> Vec<u8> {
        integrity::canonical_fields(&[
            b"task-step-compensation-v1",
            step.task_id.as_bytes(),
            step.ordinal.to_string().as_bytes(),
            step.idempotency_token.as_bytes(),
            step.adapter_id.as_bytes(),
            step.adapter_version.as_bytes(),
            task.plan_id.as_bytes(),
            task.plan_digest.as_bytes(),
            task.operation.as_str().as_bytes(),
            compensation_json.as_bytes(),
        ])
    }

    #[tokio::test]
    async fn v1_projection_migrates_without_enrolling_legacy_projection_authority() {
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
        };
        let mut legacy_none_projection = serde_json::to_value(&projection).unwrap();
        legacy_none_projection["auth"] = serde_json::json!({"type":"none"});
        sqlx::query(
            "INSERT INTO connection_projections (managed_mcp_id, link_key, projection_json, revision, updated_at_ms) VALUES ('legacy-managed', 'managed_mcp_legacy', ?, 0, 1)",
        )
        .bind(serde_json::to_string(&legacy_none_projection).unwrap())
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO managed_mcps (managed_mcp_id, mcp_id, installation_scope, state_json, revision, created_at_ms, updated_at_ms) VALUES ('legacy-auth-managed', 'legacy-auth.example', 'user', ?, 0, 1, 1)",
        )
        .bind(serde_json::to_string(&ManagedMcpState::default()).unwrap())
        .execute(&mut *transaction)
        .await
        .unwrap();
        let mut legacy_auth_projection = serde_json::to_value(&projection).unwrap();
        legacy_auth_projection["auth"] = serde_json::json!({
            "type":"api_key_header",
            "header_name":"Authorization",
            "prefix":"Bearer",
            "credential_name":"opaque-canary-handle"
        });
        sqlx::query(
            "INSERT INTO connection_projections (managed_mcp_id, link_key, projection_json, revision, updated_at_ms) VALUES ('legacy-auth-managed', 'managed_mcp_legacy_auth', ?, 0, 1)",
        )
        .bind(serde_json::to_string(&legacy_auth_projection).unwrap())
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        pool.close().await;

        let repository = match SqliteMcpPlatformRepository::open_path(&path).await {
            Ok(repository) => repository,
            Err(error) => {
                assert!(matches!(
                    error.code(),
                    McpPlatformErrorCode::IntegrityError
                        | McpPlatformErrorCode::IntegrityUnavailable
                ));
                return;
            }
        };
        let legacy_error = repository
            .get_connection_projection("legacy-managed")
            .await
            .unwrap_err();
        assert_eq!(legacy_error.code(), McpPlatformErrorCode::IntegrityError);
        let legacy_auth_error = repository
            .get_connection_projection("legacy-auth-managed")
            .await
            .unwrap_err();
        assert_eq!(
            legacy_auth_error.code(),
            McpPlatformErrorCode::IntegrityError
        );
        assert!(!legacy_auth_error
            .to_string()
            .contains("opaque-canary-handle"));
        let versions =
            sqlx::query_scalar::<_, i64>("SELECT version FROM schema_version ORDER BY version")
                .fetch_all(&repository.pool)
                .await
                .unwrap();
        assert_eq!(versions, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        for table in [
            "mcp_profiles",
            "mcp_profile_revisions",
            "mcp_profile_apply_plans",
            "mcp_profile_application_tokens",
            "mcp_profile_applications",
        ] {
            assert!(sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
            )
            .bind(table)
            .fetch_one(&repository.pool)
            .await
            .unwrap());
        }
        let managed_version_columns = sqlx::query("PRAGMA table_info(managed_versions)")
            .fetch_all(&repository.pool)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|row| row.try_get::<String, _>("name").ok())
            .collect::<Vec<_>>();
        assert!(managed_version_columns
            .iter()
            .any(|name| name == "supply_chain_evidence_json"));
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

    #[tokio::test]
    async fn legacy_projection_tampering_with_empty_or_old_digest_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-projection-tamper.db");
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
        sqlx::query(
            "INSERT INTO managed_mcps (managed_mcp_id, mcp_id, installation_scope, state_json, revision, created_at_ms, updated_at_ms) VALUES ('legacy-tamper', 'legacy-tamper.example', 'user', ?, 0, 1, 1)",
        )
        .bind(serde_json::to_string(&ManagedMcpState::default()).unwrap())
        .execute(&mut *transaction)
        .await
        .unwrap();
        let original = ConnectionProjection::RemoteHttp {
            name: "legacy-tamper".to_string(),
            description: String::new(),
            uri: "https://original.invalid/mcp".to_string(),
            timeout_seconds: Some(5),
        };
        sqlx::query(
            "INSERT INTO connection_projections (managed_mcp_id, link_key, projection_json, revision, updated_at_ms) VALUES ('legacy-tamper', 'managed_mcp_legacy_tamper', ?, 0, 1)",
        )
        .bind(crate::mcp_platform::encode_projection_config(&original).unwrap())
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        pool.close().await;

        let repository = match SqliteMcpPlatformRepository::open_path(&path).await {
            Ok(repository) => repository,
            Err(error) => {
                assert!(matches!(
                    error.code(),
                    McpPlatformErrorCode::IntegrityError
                        | McpPlatformErrorCode::IntegrityUnavailable
                ));
                return;
            }
        };
        assert_eq!(
            repository
                .get_connection_projection("legacy-tamper")
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );
        let old_digest = crate::mcp_platform::projection_config_digest(&original).unwrap();
        let tampered = ConnectionProjection::RemoteHttp {
            name: "legacy-tamper".to_string(),
            description: String::new(),
            uri: "https://tampered-secret.invalid/mcp".to_string(),
            timeout_seconds: Some(5),
        };
        let tampered_json = crate::mcp_platform::encode_projection_config(&tampered).unwrap();

        sqlx::query(
            "UPDATE connection_projections SET projection_json = ?, projection_digest = '' WHERE managed_mcp_id = 'legacy-tamper'",
        )
        .bind(&tampered_json)
        .execute(&repository.pool)
        .await
        .unwrap();
        assert_eq!(
            repository
                .get_connection_projection("legacy-tamper")
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );
        let empty_digest_service =
            crate::mcp_platform::McpPlatformService::production(Arc::new(repository.clone()));
        assert_eq!(
            empty_digest_service
                .managed_extension_provenance()
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );

        sqlx::query(
            "UPDATE connection_projections SET projection_digest = ? WHERE managed_mcp_id = 'legacy-tamper'",
        )
        .bind(old_digest)
        .execute(&repository.pool)
        .await
        .unwrap();
        assert_eq!(
            repository
                .get_connection_projection("legacy-tamper")
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );
        let service = crate::mcp_platform::McpPlatformService::production(Arc::new(repository));
        assert_eq!(
            service
                .managed_extension_provenance()
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );
    }

    #[tokio::test]
    async fn anchored_state_rejects_database_wide_rewrite() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("external-root.db");
        let original_signer = InMemoryIntegritySigner::new_for_testing([0x11; 32]);
        let repository = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            original_signer.clone(),
        )
        .await
        .unwrap();
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "external-root-managed".to_string(),
                    mcp_id: "external-root.example".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        let projection = ConnectionProjection::RemoteHttp {
            name: "external-root".to_string(),
            description: String::new(),
            uri: "https://external-root.example/mcp".to_string(),
            timeout_seconds: Some(5),
        };
        let projection_json = crate::mcp_platform::encode_projection_config(&projection).unwrap();
        let projection_digest = crate::mcp_platform::projection_config_digest(&projection).unwrap();
        sqlx::query(
            r#"INSERT INTO connection_projections(
                managed_mcp_id,link_key,projection_json,revision,updated_at_ms,projection_digest
            ) VALUES (?,?,?,0,?,?)"#,
        )
        .bind("external-root-managed")
        .bind("external-root-link")
        .bind(projection_json)
        .bind(2_i64)
        .bind(&projection_digest)
        .execute(&repository.pool)
        .await
        .unwrap();
        let mac = repository
            .integrity_signer
            .sign(
                "managed-projection",
                &managed_projection_integrity_payload(
                    "external-root-managed",
                    "external-root-link",
                    &projection_digest,
                    0,
                    None,
                    None,
                    None,
                ),
            )
            .unwrap();
        sqlx::query("INSERT INTO managed_projection_integrity(managed_mcp_id,mac) VALUES (?,?)")
            .bind("external-root-managed")
            .bind(mac)
            .execute(&repository.pool)
            .await
            .unwrap();
        let tampered = ConnectionProjection::RemoteHttp {
            name: "external-root".to_string(),
            description: "database rewrite".to_string(),
            uri: "https://attacker.invalid/mcp".to_string(),
            timeout_seconds: Some(1),
        };
        let tampered_json = crate::mcp_platform::encode_projection_config(&tampered).unwrap();
        let tampered_digest = crate::mcp_platform::projection_config_digest(&tampered).unwrap();
        sqlx::query("UPDATE connection_projections SET link_key='attacker-link',projection_json=?,projection_digest=?,revision=99,updated_at_ms=99 WHERE managed_mcp_id='external-root-managed'")
            .bind(tampered_json)
            .bind(tampered_digest)
            .execute(&repository.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE managed_projection_integrity SET mac=? WHERE managed_mcp_id='external-root-managed'")
            .bind("0".repeat(64))
            .execute(&repository.pool)
            .await
            .unwrap();
        assert_eq!(
            repository
                .get_connection_projection("external-root-managed")
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );
        if repository.verify_integrity().await.is_err() {
            return;
        }
        sqlx::query("UPDATE connection_projections SET link_key='external-root-link',projection_json=?,projection_digest=?,revision=0,updated_at_ms=2 WHERE managed_mcp_id='external-root-managed'")
            .bind(crate::mcp_platform::encode_projection_config(&projection).unwrap())
            .bind(&projection_digest)
            .execute(&repository.pool)
            .await
            .unwrap();
        let original_mac = repository
            .integrity_signer
            .sign(
                "managed-projection",
                &managed_projection_integrity_payload(
                    "external-root-managed",
                    "external-root-link",
                    &projection_digest,
                    0,
                    None,
                    None,
                    None,
                ),
            )
            .unwrap();
        sqlx::query("UPDATE managed_projection_integrity SET mac=? WHERE managed_mcp_id='external-root-managed'")
            .bind(original_mac)
            .execute(&repository.pool)
            .await
            .unwrap();
        repository
            .get_connection_projection("external-root-managed")
            .await
            .unwrap();
        repository.close().await;

        assert_eq!(
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(
                &path,
                InMemoryIntegritySigner::new_for_testing([0x22; 32]),
            )
            .await
            .unwrap_err()
            .code(),
            McpPlatformErrorCode::IntegrityError
        );

        let original =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, original_signer)
                .await
                .unwrap();
        original
            .get_connection_projection("external-root-managed")
            .await
            .unwrap();
        original.close().await;
    }

    #[tokio::test]
    async fn unavailable_provider_fails_before_bootstrap_and_leaves_database_unwritten() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("missing").join("parent");
        let path = parent.join("unavailable-bootstrap.db");
        let lock_path = integrity_lock_path(&path);
        assert!(!parent.exists());

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            integrity::unavailable_signer(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert!(!parent.exists());
        assert!(!path.exists());
        assert!(!lock_path.exists());
    }

    #[tokio::test]
    async fn unavailable_provider_never_falls_back_to_local_anchor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unavailable-existing.db");
        let signer = InMemoryIntegritySigner::new_for_testing([0x12; 32]);
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "unavailable-managed".to_string(),
                    mcp_id: "unavailable.example".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository.close().await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            integrity::unavailable_signer(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);

        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        reopened
            .get_managed_mcp("unavailable-managed")
            .await
            .unwrap();
        reopened.close().await;
    }

    #[tokio::test]
    async fn unsupported_provider_is_explicitly_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("unsupported").join("parent");
        let path = parent.join("unsupported-provider.db");
        let lock_path = integrity_lock_path(&path);
        assert!(!parent.exists());

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            integrity::unsupported_signer("remote-notary"),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert!(!parent.exists());
        assert!(!path.exists());
        assert!(!lock_path.exists());
    }

    #[tokio::test]
    async fn unsupported_provider_inputs_with_unicode_nul_and_oversized_values_are_rejected() {
        let inputs = vec![
            "供应商".to_string(),
            "unsupported\0provider".to_string(),
            "x".repeat(512),
        ];
        for (index, provider_id) in inputs.into_iter().enumerate() {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(format!("unsupported-{index}.db"));
            let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
                &path,
                integrity::unsupported_signer(provider_id),
            )
            .await
            .unwrap_err();
            assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
            assert!(!path.exists());
            assert!(!integrity_lock_path(&path).exists());
        }
    }

    #[tokio::test]
    async fn trusted_reenrollment_is_not_triggered_by_mismatched_provider_open() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("no-implicit-reenroll.db");
        let original_signer = InMemoryIntegritySigner::new_for_testing([0x13; 32]);
        let repository = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            original_signer.clone(),
        )
        .await
        .unwrap();
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "reenroll-guard".to_string(),
                    mcp_id: "reenroll.guard".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository.close().await;

        assert_eq!(
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(
                &path,
                InMemoryIntegritySigner::new_for_testing([0x23; 32]),
            )
            .await
            .unwrap_err()
            .code(),
            McpPlatformErrorCode::IntegrityError
        );

        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, original_signer)
                .await
                .unwrap();
        reopened.get_managed_mcp("reenroll-guard").await.unwrap();
        reopened.close().await;
    }

    #[tokio::test]
    async fn provider_selection_boundary_is_explicit_between_production_and_tests() {
        let production = integrity::system_signer(&"b".repeat(64), false);
        assert_eq!(production.provider_id(), "system-keyring");
        assert_eq!(
            production.assurance(),
            integrity::AnchorProviderAssurance::SystemKeyring
        );
        let test = InMemoryIntegritySigner::new_for_testing([0x14; 32]);
        assert_eq!(test.provider_id(), "in-memory-test");
        assert_eq!(
            test.assurance(),
            integrity::AnchorProviderAssurance::TestInMemory
        );
        assert_ne!(production.provider_id(), test.provider_id());
    }

    #[tokio::test]
    async fn legacy_v13_in_memory_anchor_migrates_to_provider_bound_and_reopen_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-v13-in-memory.db");
        let signer = InMemoryIntegritySigner::new_for_testing([0x54; 32]);
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "legacy-migrate".to_string(),
                    mcp_id: "legacy.migrate".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository.close().await;
        rewrite_integrity_repository_as_legacy_v13(&path, signer.as_ref()).await;
        assert!(!sqlite_table_has_column(&path, "integrity_metadata", "provider_id").await);

        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        reopened.get_managed_mcp("legacy-migrate").await.unwrap();
        let provider_id = sqlx::query_scalar::<_, String>(
            "SELECT provider_id FROM integrity_metadata WHERE singleton = 1",
        )
        .fetch_one(&reopened.pool)
        .await
        .unwrap();
        assert_eq!(provider_id, "in-memory-test");
        let provider_ids = sqlx::query_scalar::<_, Option<String>>(
            "SELECT provider_id FROM integrity_commits ORDER BY sequence",
        )
        .fetch_all(&reopened.pool)
        .await
        .unwrap();
        assert!(provider_ids
            .iter()
            .all(|provider_id| provider_id.as_deref() == Some("in-memory-test")));
        reopened.close().await;

        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        reopened.get_managed_mcp("legacy-migrate").await.unwrap();
        let provider_id = sqlx::query_scalar::<_, String>(
            "SELECT provider_id FROM integrity_metadata WHERE singleton = 1",
        )
        .fetch_one(&reopened.pool)
        .await
        .unwrap();
        assert_eq!(provider_id, "in-memory-test");
        reopened.close().await;
    }

    #[tokio::test]
    async fn legacy_v13_provider_mismatch_fails_closed_without_overwriting_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-v13-mismatch.db");
        let original_signer = InMemoryIntegritySigner::new_for_testing([0x55; 32]);
        let repository = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            original_signer.clone(),
        )
        .await
        .unwrap();
        repository.close().await;
        rewrite_integrity_repository_as_legacy_v13(&path, original_signer.as_ref()).await;
        assert!(!sqlite_table_has_column(&path, "integrity_metadata", "provider_id").await);

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            InMemoryIntegritySigner::new_for_testing([0x65; 32]),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert!(!sqlite_table_has_column(&path, "integrity_metadata", "provider_id").await);

        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, original_signer)
                .await
                .unwrap();
        reopened.close().await;
    }

    #[tokio::test]
    async fn tampered_provider_id_in_database_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("provider-id-tamper.db");
        let signer = InMemoryIntegritySigner::new_for_testing([0x56; 32]);
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        sqlx::query(
            "UPDATE integrity_metadata SET provider_id = 'system-keyring' WHERE singleton = 1",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE integrity_commits SET provider_id = 'system-keyring'")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn legacy_system_keyring_anchor_migrates_once_and_reopen_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-system-keyring.db");
        let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "legacy-system".to_string(),
                    mcp_id: "legacy.system".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository.close().await;
        let path_binding = database_path_binding(&path).unwrap();
        let signer = integrity::system_signer(&path_binding, false);
        rewrite_integrity_repository_as_legacy_v13(&path, signer.as_ref()).await;
        integrity::rewrite_system_anchor_as_legacy_for_testing(&path_binding).unwrap();

        let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        reopened.get_managed_mcp("legacy-system").await.unwrap();
        let provider_id = sqlx::query_scalar::<_, String>(
            "SELECT provider_id FROM integrity_metadata WHERE singleton = 1",
        )
        .fetch_one(&reopened.pool)
        .await
        .unwrap();
        assert_eq!(provider_id, "system-keyring");
        reopened.close().await;

        let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        reopened.get_managed_mcp("legacy-system").await.unwrap();
        reopened.close().await;
        integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding).unwrap();
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn malformed_persisted_system_anchor_provider_ids_fail_closed_without_rewriting_database()
    {
        for (index, provider_id) in [
            "供应商".to_string(),
            "system-keyring\0evil".to_string(),
            "x".repeat(512),
        ]
        .into_iter()
        .enumerate()
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory
                .path()
                .join(format!("invalid-provider-id-{index}.db"));
            let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
            repository.close().await;
            let path_binding = database_path_binding(&path).unwrap();
            let signer = integrity::system_signer(&path_binding, false);
            rewrite_integrity_repository_as_legacy_v13(&path, signer.as_ref()).await;
            integrity::overwrite_system_anchor_provider_id_for_testing(&path_binding, &provider_id)
                .unwrap();

            let error = SqliteMcpPlatformRepository::open_path(&path)
                .await
                .unwrap_err();
            assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
            assert!(!sqlite_table_has_column(&path, "integrity_metadata", "provider_id").await);
            let _ = integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding);
        }
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn v14_system_anchor_payloads_fail_closed_without_rewriting_keyring_or_database() {
        for index in 0..INVALID_SYSTEM_ANCHOR_PAYLOAD_CASE_COUNT {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(format!("invalid-v14-{index}.db"));
            let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
            repository.close().await;
            let path_binding = database_path_binding(&path).unwrap();
            let raw_valid_payload =
                integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();
            let valid_payload =
                serde_json::from_str::<serde_json::Value>(&raw_valid_payload).unwrap();
            let cases = invalid_system_anchor_payloads(&valid_payload, &raw_valid_payload);
            let (case, payload) = &cases[index];
            let provider_state_before = read_integrity_provider_binding_state(&path).await;
            integrity::write_raw_system_anchor_for_testing(&path_binding, payload).unwrap();
            assert_eq!(
                integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap(),
                payload.as_str()
            );

            let error = SqliteMcpPlatformRepository::open_path(&path)
                .await
                .unwrap_err();
            assert_eq!(
                error.code(),
                McpPlatformErrorCode::IntegrityError,
                "case {case} failed with unexpected error"
            );
            assert_eq!(
                integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap(),
                payload.as_str()
            );
            assert_eq!(
                read_integrity_provider_binding_state(&path).await,
                provider_state_before
            );
            assert_no_trusted_reenrollment_quarantine(directory.path(), &path);

            let retry = SqliteMcpPlatformRepository::open_path(&path)
                .await
                .unwrap_err();
            assert_eq!(retry.code(), McpPlatformErrorCode::IntegrityError);
            assert_eq!(
                integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap(),
                payload.as_str()
            );
            let _ = integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding);
        }
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn legacy_v13_audit_payloads_fail_closed_without_rewriting_keyring_or_database() {
        let audit_cases = [
            "audit-v2-like-wrong-version-evil-provider",
            "audit-v2-like-missing-provider",
        ];
        for (index, case) in audit_cases.into_iter().enumerate() {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(format!("invalid-v13-{index}.db"));
            let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
            repository.close().await;
            let path_binding = database_path_binding(&path).unwrap();
            let signer = integrity::system_signer(&path_binding, false);
            let raw_valid_payload =
                integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();
            let valid_payload =
                serde_json::from_str::<serde_json::Value>(&raw_valid_payload).unwrap();
            let payload = invalid_system_anchor_payloads(&valid_payload, &raw_valid_payload)
                .into_iter()
                .find(|(name, _)| *name == case)
                .unwrap()
                .1;
            rewrite_integrity_repository_as_legacy_v13(&path, signer.as_ref()).await;
            assert!(!sqlite_table_has_column(&path, "integrity_metadata", "provider_id").await);

            integrity::write_raw_system_anchor_for_testing(&path_binding, &payload).unwrap();
            assert_eq!(
                integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap(),
                payload
            );

            let error = SqliteMcpPlatformRepository::open_path(&path)
                .await
                .unwrap_err();
            assert_eq!(
                error.code(),
                McpPlatformErrorCode::IntegrityError,
                "case {case} failed with unexpected error"
            );
            assert_eq!(
                integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap(),
                payload
            );
            assert!(!sqlite_table_has_column(&path, "integrity_metadata", "provider_id").await);
            assert_no_trusted_reenrollment_quarantine(directory.path(), &path);

            let retry = SqliteMcpPlatformRepository::open_path(&path)
                .await
                .unwrap_err();
            assert_eq!(retry.code(), McpPlatformErrorCode::IntegrityError);
            assert_eq!(
                integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap(),
                payload
            );
            let _ = integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding);
        }
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn system_and_in_memory_anchor_cross_use_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("system-cross-use.db");
        let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        repository.close().await;
        let path_binding = database_path_binding(&path).unwrap();
        let raw_before = integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();
        let in_memory_signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x58; 32],
            path_binding.clone(),
        );
        let seed_path = directory.path().join("system-cross-use-seed.db");
        let seeded = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &seed_path,
            in_memory_signer.clone(),
        )
        .await
        .unwrap();
        seeded.close().await;

        let error =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, in_memory_signer)
                .await
                .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap(),
            raw_before
        );

        let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        reopened.close().await;
        integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding).unwrap();
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn legacy_system_anchor_rewrite_failure_rolls_back_database_and_retries_safely() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-system-rewrite-failure.db");
        let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        repository.close().await;
        let path_binding = database_path_binding(&path).unwrap();
        let signer = integrity::system_signer(&path_binding, false);
        rewrite_integrity_repository_as_legacy_v13(&path, signer.as_ref()).await;
        integrity::rewrite_system_anchor_as_legacy_for_testing(&path_binding).unwrap();
        let legacy_raw = integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();

        let _hook = install_after_legacy_provider_binding_database_rewrite_hook(|| {
            Err(crate::mcp_platform::error::McpPlatformError::new(
                McpPlatformErrorCode::RepositoryUnavailable,
                "forced legacy provider rewrite failure for testing",
            ))
        });
        let error = SqliteMcpPlatformRepository::open_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RepositoryUnavailable);
        let migrated_raw = integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();
        assert_ne!(migrated_raw, legacy_raw);
        assert!(
            migrated_raw.contains("\"format_version\":2")
                || migrated_raw.contains("\"format_version\":3")
        );
        assert!(!sqlite_table_has_column(&path, "integrity_metadata", "provider_id").await);

        let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        let provider_id = sqlx::query_scalar::<_, String>(
            "SELECT provider_id FROM integrity_metadata WHERE singleton = 1",
        )
        .fetch_one(&reopened.pool)
        .await
        .unwrap();
        assert_eq!(provider_id, "system-keyring");
        reopened.close().await;
        integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding).unwrap();
    }

    #[tokio::test]
    async fn unavailable_existing_repository_open_is_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unavailable-existing-open.db");
        let repository = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            InMemoryIntegritySigner::new_for_testing([0x15; 32]),
        )
        .await
        .unwrap();
        repository.close().await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            integrity::unavailable_signer(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
    }

    #[tokio::test]
    async fn unavailable_provider_blocks_existing_database_without_rewriting_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unavailable-metadata.db");
        let signer = InMemoryIntegritySigner::new_for_testing([0x16; 32]);
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let before = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM integrity_metadata")
            .fetch_one(&repository.pool)
            .await
            .unwrap();
        repository.close().await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            integrity::unavailable_signer(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);

        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let after = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM integrity_metadata")
            .fetch_one(&reopened.pool)
            .await
            .unwrap();
        assert_eq!(before, after);
        reopened.close().await;
    }

    #[tokio::test]
    async fn unavailable_provider_on_existing_database_is_explicitly_unavailable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unavailable-existing-signal.db");
        let repository = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            InMemoryIntegritySigner::new_for_testing([0x17; 32]),
        )
        .await
        .unwrap();
        repository.close().await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            integrity::unavailable_signer(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert!(error
            .to_string()
            .contains("managed MCP integrity anchor is unavailable"));
    }

    #[tokio::test]
    async fn unavailable_provider_rejects_before_any_new_database_tables_exist() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unavailable-tables.db");

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            integrity::unavailable_signer(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn unavailable_provider_does_not_create_database_lock_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unavailable-lock.db");
        let lock_path = integrity_lock_path(&path);

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            integrity::unavailable_signer(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert!(!lock_path.exists());
    }

    #[tokio::test]
    async fn unavailable_provider_does_not_mutate_existing_database_path_binding() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unavailable-binding.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x18; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let before = sqlx::query_scalar::<_, String>(
            "SELECT path_binding FROM integrity_metadata WHERE singleton=1",
        )
        .fetch_one(&repository.pool)
        .await
        .unwrap();
        repository.close().await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            integrity::unavailable_signer(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);

        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let after = sqlx::query_scalar::<_, String>(
            "SELECT path_binding FROM integrity_metadata WHERE singleton=1",
        )
        .fetch_one(&reopened.pool)
        .await
        .unwrap();
        assert_eq!(before, after);
        reopened.close().await;
    }

    #[tokio::test]
    async fn default_in_memory_signer_file_backed_metadata_uses_real_path_binding() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("default-path-bound.db");
        let expected_path_binding = database_path_binding(&path).unwrap();
        let signer = InMemoryIntegritySigner::new_for_testing([0x70; 32]);
        assert_eq!(
            signer.identity().unwrap().path_binding,
            "in-memory-test-database"
        );

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let stored_path_binding = sqlx::query_scalar::<_, String>(
            "SELECT path_binding FROM integrity_metadata WHERE singleton = 1",
        )
        .fetch_one(&repository.pool)
        .await
        .unwrap();
        assert_eq!(stored_path_binding, expected_path_binding);
        assert_eq!(stored_path_binding.len(), 64);
        repository.close().await;

        let reopened = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap();
        reopened.close().await;
    }

    #[tokio::test]
    async fn explicit_path_bound_in_memory_signer_is_not_rebound_for_matching_file_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("explicit-path-bound.db");
        let expected_path_binding = database_path_binding(&path).unwrap();
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x71; 32],
            expected_path_binding.clone(),
        );
        let normalized =
            normalize_file_backed_integrity_signer(signer.clone(), &expected_path_binding).unwrap();
        assert!(Arc::ptr_eq(&normalized, &signer));

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let stored_path_binding = sqlx::query_scalar::<_, String>(
            "SELECT path_binding FROM integrity_metadata WHERE singleton = 1",
        )
        .fetch_one(&repository.pool)
        .await
        .unwrap();
        assert_eq!(stored_path_binding, expected_path_binding);
        repository.close().await;

        let reopened = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap();
        reopened.close().await;
    }

    #[tokio::test]
    async fn default_in_memory_signer_checkpoint_state_is_scoped_per_file_path() {
        let directory = tempfile::tempdir().unwrap();
        let path_a = directory.path().join("scoped-path-a.db");
        let path_b = directory.path().join("scoped-path-b.db");
        let expected_a = database_path_binding(&path_a).unwrap();
        let expected_b = database_path_binding(&path_b).unwrap();
        let signer = InMemoryIntegritySigner::new_for_testing([0x72; 32]);
        let signer_key = path_bound_test_signer_registry_key(&signer);

        {
            let repository_a = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
                &path_a,
                signer.clone(),
            )
            .await
            .unwrap();
            repository_a
                .create_managed_mcp(
                    &NewManagedMcp {
                        managed_mcp_id: "scoped-path-a-1".to_string(),
                        mcp_id: "scoped-path-a.example".to_string(),
                        installation_scope: "user".to_string(),
                    },
                    1,
                )
                .await
                .unwrap();
            repository_a.close().await;
            assert_eq!(
                path_bound_test_signer_registry_entry_count_for_testing(signer_key),
                1
            );
        }
        assert_eq!(
            path_bound_test_signer_registry_entry_count_for_testing(signer_key),
            0
        );

        {
            let repository_b = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
                &path_b,
                signer.clone(),
            )
            .await
            .unwrap();
            repository_b
                .create_managed_mcp(
                    &NewManagedMcp {
                        managed_mcp_id: "scoped-path-b-1".to_string(),
                        mcp_id: "scoped-path-b.example".to_string(),
                        installation_scope: "user".to_string(),
                    },
                    1,
                )
                .await
                .unwrap();
            repository_b.close().await;
            assert_eq!(
                path_bound_test_signer_registry_entry_count_for_testing(signer_key),
                1
            );
        }
        assert_eq!(
            path_bound_test_signer_registry_entry_count_for_testing(signer_key),
            0
        );

        {
            let reopened_a = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
                &path_a,
                signer.clone(),
            )
            .await
            .unwrap();
            assert_eq!(
                reopened_a
                    .projection_writer_repository_identity()
                    .unwrap()
                    .path_binding,
                expected_a
            );
            reopened_a
                .create_managed_mcp(
                    &NewManagedMcp {
                        managed_mcp_id: "scoped-path-a-2".to_string(),
                        mcp_id: "scoped-path-a-reopen.example".to_string(),
                        installation_scope: "user".to_string(),
                    },
                    2,
                )
                .await
                .unwrap();
            reopened_a.close().await;
        }
        assert_eq!(
            path_bound_test_signer_registry_entry_count_for_testing(signer_key),
            0
        );
        {
            let reopened_b = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
                &path_b,
                signer.clone(),
            )
            .await
            .unwrap();
            assert_eq!(
                reopened_b
                    .projection_writer_repository_identity()
                    .unwrap()
                    .path_binding,
                expected_b
            );
            reopened_b
                .create_managed_mcp(
                    &NewManagedMcp {
                        managed_mcp_id: "scoped-path-b-2".to_string(),
                        mcp_id: "scoped-path-b-reopen.example".to_string(),
                        installation_scope: "user".to_string(),
                    },
                    2,
                )
                .await
                .unwrap();
            reopened_b.close().await;
        }
        assert_eq!(
            path_bound_test_signer_registry_entry_count_for_testing(signer_key),
            0
        );
    }

    #[test]
    fn path_bound_test_signer_registry_cleans_stale_entries() {
        let signer = InMemoryIntegritySigner::new_for_testing([0x73; 32]);
        let signer_key = path_bound_test_signer_registry_key(&signer);
        let expected_path_binding = "7".repeat(64);
        {
            let rebound =
                normalize_file_backed_integrity_signer(signer.clone(), &expected_path_binding)
                    .unwrap();
            assert!(!Arc::ptr_eq(&rebound, &signer));
            assert!(path_bound_test_signer_registry_contains_for_testing(
                signer_key,
                &expected_path_binding
            ));
        }
        assert!(!path_bound_test_signer_registry_contains_for_testing(
            signer_key,
            &expected_path_binding
        ));
        assert_eq!(
            path_bound_test_signer_registry_entry_count_for_testing(signer_key),
            0
        );
        drop(signer);
    }

    #[tokio::test]
    async fn scoped_checkpoint_state_recycle_allows_same_signer_same_path_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("scoped-recycle-reopen.db");
        let signer = InMemoryIntegritySigner::new_for_testing([0x74; 32]);
        let seeded = seed_repository_with_recycled_checkpoint_state(
            &path,
            signer.clone(),
            "scoped-recycle-reopen",
            "scoped.recycle.reopen.example",
        )
        .await;

        {
            let reopened =
                SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                    .await
                    .unwrap();
            assert_eq!(
                reopened
                    .projection_writer_repository_identity()
                    .unwrap()
                    .path_binding,
                seeded.path_binding
            );
            reopened
                .get_managed_mcp("scoped-recycle-reopen")
                .await
                .unwrap();
            reopened.close().await;
            assert_eq!(
                path_bound_test_signer_registry_entry_count_for_testing(seeded.signer_key),
                1
            );
        }

        assert_eq!(
            path_bound_test_signer_registry_entry_count_for_testing(seeded.signer_key),
            0
        );
        assert_eq!(
            sqlite_integrity_commit_count(&path).await,
            seeded.commit_count
        );
    }

    #[tokio::test]
    async fn scoped_checkpoint_state_recycle_then_tamper_variants_fail_closed_without_repair() {
        for (index, case) in [
            ScopedCheckpointTamperCase::MetadataRoot,
            ScopedCheckpointTamperCase::MetadataCommitMac,
            ScopedCheckpointTamperCase::CommitMac,
            ScopedCheckpointTamperCase::ParentRoot,
            ScopedCheckpointTamperCase::TruncateTailCommit,
        ]
        .into_iter()
        .enumerate()
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory
                .path()
                .join(format!("recycled-checkpoint-tamper-{index}.db"));
            let signer = InMemoryIntegritySigner::new_for_testing([0x80 + index as u8; 32]);
            let seeded = seed_repository_with_recycled_checkpoint_state(
                &path,
                signer.clone(),
                &format!("recycled-checkpoint-{index}"),
                &format!("recycled.checkpoint.{index}.example"),
            )
            .await;

            match case {
                ScopedCheckpointTamperCase::MetadataRoot => {
                    let tampered_root = "a".repeat(64);
                    let pool = existing_repository_pool(&path).await;
                    sqlx::query("UPDATE integrity_metadata SET root = ? WHERE singleton = 1")
                        .bind(&tampered_root)
                        .execute(&pool)
                        .await
                        .unwrap();
                    pool.close().await;

                    let error =
                        SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                            .await
                            .unwrap_err();
                    assert_eq!(
                        error.code(),
                        McpPlatformErrorCode::IntegrityError,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        path_bound_test_signer_registry_entry_count_for_testing(seeded.signer_key),
                        0,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        sqlite_string_scalar(
                            &path,
                            "SELECT root FROM integrity_metadata WHERE singleton = 1"
                        )
                        .await,
                        tampered_root,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        sqlite_i64_scalar(
                            &path,
                            "SELECT sequence FROM integrity_metadata WHERE singleton = 1"
                        )
                        .await,
                        seeded.metadata_sequence,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        sqlite_integrity_commit_count(&path).await,
                        seeded.commit_count,
                        "case {}",
                        case.name()
                    );
                }
                ScopedCheckpointTamperCase::MetadataCommitMac => {
                    let tampered_commit_mac = "b".repeat(64);
                    let pool = existing_repository_pool(&path).await;
                    sqlx::query("UPDATE integrity_metadata SET commit_mac = ? WHERE singleton = 1")
                        .bind(&tampered_commit_mac)
                        .execute(&pool)
                        .await
                        .unwrap();
                    pool.close().await;

                    let error =
                        SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                            .await
                            .unwrap_err();
                    assert_eq!(
                        error.code(),
                        McpPlatformErrorCode::IntegrityError,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        path_bound_test_signer_registry_entry_count_for_testing(seeded.signer_key),
                        0,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        sqlite_string_scalar(
                            &path,
                            "SELECT commit_mac FROM integrity_metadata WHERE singleton = 1"
                        )
                        .await,
                        tampered_commit_mac,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        sqlite_i64_scalar(
                            &path,
                            "SELECT sequence FROM integrity_metadata WHERE singleton = 1"
                        )
                        .await,
                        seeded.metadata_sequence,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        sqlite_integrity_commit_count(&path).await,
                        seeded.commit_count,
                        "case {}",
                        case.name()
                    );
                }
                ScopedCheckpointTamperCase::CommitMac => {
                    let tampered_commit_mac = "c".repeat(64);
                    let pool = existing_repository_pool(&path).await;
                    sqlx::query("UPDATE integrity_commits SET commit_mac = ? WHERE sequence = ?")
                        .bind(&tampered_commit_mac)
                        .bind(seeded.tail_sequence)
                        .execute(&pool)
                        .await
                        .unwrap();
                    pool.close().await;

                    let error =
                        SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                            .await
                            .unwrap_err();
                    assert_eq!(
                        error.code(),
                        McpPlatformErrorCode::IntegrityError,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        path_bound_test_signer_registry_entry_count_for_testing(seeded.signer_key),
                        0,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        sqlite_string_scalar(
                            &path,
                            &format!(
                                "SELECT commit_mac FROM integrity_commits WHERE sequence = {}",
                                seeded.tail_sequence
                            )
                        )
                        .await,
                        tampered_commit_mac,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        sqlite_i64_scalar(
                            &path,
                            "SELECT sequence FROM integrity_metadata WHERE singleton = 1"
                        )
                        .await,
                        seeded.metadata_sequence,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        sqlite_integrity_commit_count(&path).await,
                        seeded.commit_count,
                        "case {}",
                        case.name()
                    );
                }
                ScopedCheckpointTamperCase::ParentRoot => {
                    let tampered_parent_root = "d".repeat(64);
                    let pool = existing_repository_pool(&path).await;
                    sqlx::query("UPDATE integrity_commits SET parent_root = ? WHERE sequence = ?")
                        .bind(&tampered_parent_root)
                        .bind(seeded.tail_sequence)
                        .execute(&pool)
                        .await
                        .unwrap();
                    pool.close().await;

                    let error =
                        SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                            .await
                            .unwrap_err();
                    assert_eq!(
                        error.code(),
                        McpPlatformErrorCode::IntegrityError,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        path_bound_test_signer_registry_entry_count_for_testing(seeded.signer_key),
                        0,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        sqlite_string_scalar(
                            &path,
                            &format!(
                                "SELECT parent_root FROM integrity_commits WHERE sequence = {}",
                                seeded.tail_sequence
                            )
                        )
                        .await,
                        tampered_parent_root,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        sqlite_i64_scalar(
                            &path,
                            "SELECT sequence FROM integrity_metadata WHERE singleton = 1"
                        )
                        .await,
                        seeded.metadata_sequence,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        sqlite_integrity_commit_count(&path).await,
                        seeded.commit_count,
                        "case {}",
                        case.name()
                    );
                }
                ScopedCheckpointTamperCase::TruncateTailCommit => {
                    let pool = existing_repository_pool(&path).await;
                    sqlx::query("DELETE FROM integrity_commits WHERE sequence = ?")
                        .bind(seeded.tail_sequence)
                        .execute(&pool)
                        .await
                        .unwrap();
                    pool.close().await;

                    let error =
                        SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                            .await
                            .unwrap_err();
                    assert_eq!(
                        error.code(),
                        McpPlatformErrorCode::IntegrityError,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        path_bound_test_signer_registry_entry_count_for_testing(seeded.signer_key),
                        0,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        sqlite_i64_scalar(
                            &path,
                            "SELECT sequence FROM integrity_metadata WHERE singleton = 1"
                        )
                        .await,
                        seeded.metadata_sequence,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        sqlite_integrity_commit_count(&path).await,
                        seeded.commit_count - 1,
                        "case {}",
                        case.name()
                    );
                    assert_eq!(
                        sqlite_i64_scalar(
                            &path,
                            &format!(
                                "SELECT COUNT(*) FROM integrity_commits WHERE sequence = {}",
                                seeded.tail_sequence
                            )
                        )
                        .await,
                        0,
                        "case {}",
                        case.name()
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn tampered_existing_database_fails_before_creating_a_new_lock_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tampered-existing.db");
        let lock_path = integrity_lock_path(&path);
        let signer = InMemoryIntegritySigner::new_for_testing([0x19; 32]);
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        if lock_path.exists() {
            std::fs::remove_file(&lock_path).unwrap();
        }

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        sqlx::query("UPDATE integrity_metadata SET root = 'tampered-root' WHERE singleton = 1")
            .execute(&pool)
            .await
            .unwrap();
        let before = sqlx::query_scalar::<_, String>(
            "SELECT root FROM integrity_metadata WHERE singleton = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        pool.close().await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert!(!lock_path.exists());

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        let after = sqlx::query_scalar::<_, String>(
            "SELECT root FROM integrity_metadata WHERE singleton = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(before, after);
        pool.close().await;
    }

    #[tokio::test]
    async fn preflight_verified_database_replacement_is_rejected_before_locked_migration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("replacement-race.db");
        let replacement = directory.path().join("replacement-blank.db");
        let lock_path = integrity_lock_path(&path);
        let signer = InMemoryIntegritySigner::new_for_testing([0x1a; 32]);
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        if lock_path.exists() {
            std::fs::remove_file(&lock_path).unwrap();
        }
        let blank_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&replacement)
                    .create_if_missing(true)
                    .busy_timeout(Duration::from_secs(30)),
            )
            .await
            .unwrap();
        blank_pool.close().await;
        let _hook = install_after_preflight_before_lock_hook(path.clone(), {
            let path = path.clone();
            let replacement = replacement.clone();
            move |current_path| {
                assert_eq!(current_path, path.as_path());
                for suffix in ["-wal", "-shm"] {
                    let sidecar = path_with_suffix(current_path, suffix);
                    if sidecar.exists() {
                        std::fs::remove_file(sidecar).unwrap();
                    }
                }
                std::fs::remove_file(current_path).unwrap();
                std::fs::copy(&replacement, current_path).unwrap();
            }
        });

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert!(!lock_path.exists());
        assert_eq!(sqlite_tables(&path).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn new_repository_parent_binding_drift_after_preflight_is_rejected_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let linked_parent = directory.path().join("linked-parent");
        let physical_parent_a = directory.path().join("physical-a");
        let physical_parent_b = directory.path().join("physical-b");
        std::fs::create_dir_all(&physical_parent_a).unwrap();
        std::fs::create_dir_all(&physical_parent_b).unwrap();
        create_parent_link(&linked_parent, &physical_parent_a);

        let path = linked_parent.join("binding-drift.db");
        let lock_path = integrity_lock_path(&path);
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x1c; 32],
            database_path_binding(&path).unwrap(),
        );
        let _hook = install_after_preflight_before_lock_hook(path.clone(), {
            let path = path.clone();
            let linked_parent = linked_parent.clone();
            let physical_parent_b = physical_parent_b.clone();
            move |current_path| {
                assert_eq!(current_path, path.as_path());
                std::fs::remove_dir(&linked_parent).unwrap();
                create_parent_link(&linked_parent, &physical_parent_b);
            }
        });

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert!(!lock_path.exists());
        assert!(!path.exists());
        assert!(!physical_parent_b.join("binding-drift.db").exists());
    }

    #[tokio::test]
    async fn begin_immediate_holds_sidecar_lock_for_transaction() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("begin-immediate-lock.db");
        let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        let lock_path = repository.database_lock_path.clone().unwrap();

        let transaction = repository.begin_immediate().await.unwrap();
        let contended = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        let error = contended.try_lock_exclusive().unwrap_err();
        assert_lock_contention(error);
        drop(contended);

        transaction.rollback().await.unwrap();

        let released = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        released.try_lock_exclusive().unwrap();
        released.unlock().unwrap();
        repository.close().await;
    }

    #[tokio::test]
    async fn bootstrap_publish_failure_cleans_final_path_before_retry() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("publish-failure.db");
        let lock_path = integrity_lock_path(&path);
        let signer = InMemoryIntegritySigner::new_for_testing([0x1b; 32]);

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            PublishFailingSigner::new(signer.clone()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert!(!path.exists());
        assert!(!path_with_suffix(&path, "-journal").exists());
        assert!(!path_with_suffix(&path, "-wal").exists());
        assert!(!path_with_suffix(&path, "-shm").exists());
        assert_lock_sidecar_released(&lock_path);

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        repository.close().await;
    }

    #[tokio::test]
    async fn bootstrap_publish_then_error_cleans_stage_and_forces_future_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("publish-then-error.db");
        let lock_path = integrity_lock_path(&path);
        let signer = InMemoryIntegritySigner::new_for_testing([0x1d; 32]);

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            PublishThenErrorSigner::new(signer.clone()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert!(signer.checkpoint().unwrap().is_some());
        assert!(!path.exists());
        assert!(!path_with_suffix(&path, "-journal").exists());
        assert!(!path_with_suffix(&path, "-wal").exists());
        assert!(!path_with_suffix(&path, "-shm").exists());
        assert_lock_sidecar_released(&lock_path);

        let retry = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(retry.code(), McpPlatformErrorCode::IntegrityError);
        assert!(!path.exists());
        assert_lock_sidecar_released(&lock_path);
    }

    #[tokio::test]
    async fn post_activation_failure_scrubs_final_database_and_forces_future_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("post-activation-failure.db");
        let lock_path = integrity_lock_path(&path);
        let signer = InMemoryIntegritySigner::new_for_testing([0x1e; 32]);
        let _hook = install_after_activation_before_final_open_hook(
            path.clone(),
            |final_path, staged_path| {
                assert!(!staged_path.exists());
                assert!(final_path.exists());
                Err(repository_unavailable())
            },
        );

        let error =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RepositoryUnavailable);
        assert!(signer.checkpoint().unwrap().is_some());
        assert!(!path.exists());
        assert!(!path_with_suffix(&path, "-journal").exists());
        assert!(!path_with_suffix(&path, "-wal").exists());
        assert!(!path_with_suffix(&path, "-shm").exists());
        assert_lock_sidecar_released(&lock_path);

        let retry = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(retry.code(), McpPlatformErrorCode::IntegrityError);
        assert!(!path.exists());
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn trusted_reenrollment_identity_failure_restores_original_database_and_future_open_fails_closed(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("trusted-reenroll-identity-failure.db");
        seed_trusted_reenrollment_repository(
            &path,
            "trusted-reenroll-identity-failure",
            "trusted.reenroll.identity.failure",
        )
        .await;
        let path_binding = database_path_binding(&path).unwrap();
        seed_trusted_reenrollment_sidecars(&path);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);

        let _guard = integrity::fail_next_system_signer_identity(path_binding);
        let error = SqliteMcpPlatformRepository::trusted_reenroll_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);
        assert_no_trusted_reenrollment_quarantine(directory.path(), &path);

        let retry = SqliteMcpPlatformRepository::open_path(&path)
            .await
            .unwrap_err();
        assert_eq!(retry.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn trusted_reenrollment_publish_failure_restores_original_database_and_future_open_fails_closed(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trusted-reenroll-publish-failure.db");
        seed_trusted_reenrollment_repository(
            &path,
            "trusted-reenroll-publish-failure",
            "trusted.reenroll.publish.failure",
        )
        .await;
        let path_binding = database_path_binding(&path).unwrap();
        seed_trusted_reenrollment_sidecars(&path);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);

        let _guard = integrity::fail_next_system_signer_publish(path_binding);
        let error = SqliteMcpPlatformRepository::trusted_reenroll_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);
        assert_no_trusted_reenrollment_quarantine(directory.path(), &path);

        let retry = SqliteMcpPlatformRepository::open_path(&path)
            .await
            .unwrap_err();
        assert_eq!(retry.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn trusted_reenrollment_publish_then_error_restores_original_database_and_future_open_fails_closed(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("trusted-reenroll-publish-then-error.db");
        seed_trusted_reenrollment_repository(
            &path,
            "trusted-reenroll-publish-then-error",
            "trusted.reenroll.publish.then.error",
        )
        .await;
        let path_binding = database_path_binding(&path).unwrap();
        seed_trusted_reenrollment_sidecars(&path);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);

        let _guard = integrity::fail_next_system_signer_publish_then_error(path_binding.clone());
        let error = SqliteMcpPlatformRepository::trusted_reenroll_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);
        assert_no_trusted_reenrollment_quarantine(directory.path(), &path);

        let retry = SqliteMcpPlatformRepository::open_path(&path)
            .await
            .unwrap_err();
        assert_eq!(retry.code(), McpPlatformErrorCode::IntegrityError);
        integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding).unwrap();
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn trusted_reenrollment_post_activation_failure_returns_rollback_incomplete_and_future_open_fails_closed(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("trusted-reenroll-post-activation-failure.db");
        seed_trusted_reenrollment_repository(
            &path,
            "trusted-reenroll-post-activation-failure",
            "trusted.reenroll.post.activation.failure",
        )
        .await;
        let path_binding = database_path_binding(&path).unwrap();
        seed_trusted_reenrollment_sidecars(&path);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);
        let _hook = install_after_activation_before_final_open_hook(
            path.clone(),
            |final_path, staged_path| {
                assert!(!staged_path.exists());
                assert!(final_path.exists());
                std::fs::remove_file(final_path).unwrap();
                std::fs::write(final_path, b"external-db").unwrap();
                std::fs::write(
                    path_with_suffix(final_path, "-journal"),
                    b"external-journal",
                )
                .unwrap();
                std::fs::write(path_with_suffix(final_path, "-wal"), b"external-wal").unwrap();
                std::fs::write(path_with_suffix(final_path, "-shm"), b"external-shm").unwrap();
                Err(repository_unavailable())
            },
        );

        let error = SqliteMcpPlatformRepository::trusted_reenroll_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RollbackIncomplete);
        assert!(path.exists());
        assert_file_contents(&path, b"external-db");
        assert_file_contents(&path_with_suffix(&path, "-journal"), b"external-journal");
        assert_file_contents(&path_with_suffix(&path, "-wal"), b"external-wal");
        assert_file_contents(&path_with_suffix(&path, "-shm"), b"external-shm");
        let quarantine_prefix = "trusted-reenroll-post-activation-failure.db.quarantine-";
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(quarantine_prefix)));
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(quarantine_prefix) && name.ends_with("-journal")));
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(quarantine_prefix) && name.ends_with("-wal")));
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(quarantine_prefix) && name.ends_with("-shm")));

        let retry = SqliteMcpPlatformRepository::open_path(&path)
            .await
            .unwrap_err();
        assert_eq!(retry.code(), McpPlatformErrorCode::IntegrityError);
        let _ = integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding);
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn trusted_reenrollment_restore_failure_after_post_activation_failure_is_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("trusted-reenroll-post-activation-restore-failure.db");
        seed_trusted_reenrollment_repository(
            &path,
            "trusted-reenroll-post-activation-restore-failure",
            "trusted.reenroll.post.activation.restore.failure",
        )
        .await;
        let path_binding = database_path_binding(&path).unwrap();
        seed_trusted_reenrollment_sidecars(&path);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);
        let _hook =
            install_after_activation_before_final_open_hook(path.clone(), |final_path, _| {
                std::fs::remove_file(final_path).unwrap();
                Err(repository_unavailable())
            });
        let _guard =
            install_trusted_reenrollment_rename_failures(vec![TrustedReenrollmentRenameFailure {
                phase: TrustedReenrollmentRenamePhase::Restore,
                source: None,
                destination: Some(path.clone()),
            }]);

        let error = SqliteMcpPlatformRepository::trusted_reenroll_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RollbackIncomplete);
        assert!(!path.exists());
        assert_trusted_reenrollment_sidecars_absent(&path);
        let quarantine_prefix = "trusted-reenroll-post-activation-restore-failure.db.quarantine-";
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(quarantine_prefix)));
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(quarantine_prefix) && name.ends_with("-journal")));
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(quarantine_prefix) && name.ends_with("-wal")));
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(quarantine_prefix) && name.ends_with("-shm")));

        let retry = SqliteMcpPlatformRepository::open_path(&path)
            .await
            .unwrap_err();
        assert_eq!(retry.code(), McpPlatformErrorCode::IntegrityError);
        integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding).unwrap();
    }

    #[tokio::test]
    async fn trusted_reenrollment_half_activation_cleanup_restores_quarantined_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trusted-reenroll-half-activation.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x62; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "trusted-reenroll-half-activation".to_string(),
                    mcp_id: "trusted.reenroll.half.activation".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository.close().await;
        seed_trusted_reenrollment_sidecars(&path);
        let quarantine = path_with_suffix(&path, ".quarantine-half-activation");
        let moved_paths = sqlite_database_and_sidecar_paths(&path)
            .into_iter()
            .zip(sqlite_database_and_sidecar_paths(&quarantine))
            .map(|(original, quarantined)| {
                std::fs::rename(&original, &quarantined).unwrap();
                (quarantined, original)
            })
            .collect::<Vec<_>>();
        let staged = directory
            .path()
            .join("trusted-reenroll-half-activation.staged");
        std::fs::write(&staged, b"new-staged-db").unwrap();
        std::fs::write(path_with_suffix(&staged, "-wal"), b"new-staged-wal").unwrap();
        let _hook =
            install_move_file_source_unlink_failure_hook(staged.clone(), |_, final_path| {
                std::fs::write(path_with_suffix(final_path, "-shm"), b"new-final-shm").unwrap();
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "source unlink failed after final creation",
                ))
            });
        let activation_failure = activate_staged_database(&staged, &path).unwrap_err();
        assert!(activation_failure.final_created);
        let failure = FileBackedBootstrapFailure::created_final(
            activation_failure.error,
            staged.clone(),
            path.clone(),
        );

        rollback_trusted_reenrollment_bootstrap_failure(&failure, &moved_paths, || Ok(())).unwrap();

        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);
        assert_file_contents(&path_with_suffix(&path, "-journal"), b"journal");
        assert_file_contents(&path_with_suffix(&path, "-wal"), b"wal");
        assert_file_contents(&path_with_suffix(&path, "-shm"), b"shm");
        assert!(!staged.exists());
        assert_trusted_reenrollment_sidecars_absent(&staged);
        assert_no_trusted_reenrollment_quarantine(directory.path(), &path);

        let reopened = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap();
        reopened
            .get_managed_mcp("trusted-reenroll-half-activation")
            .await
            .unwrap();
        reopened.close().await;
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn trusted_reenrollment_quarantine_target_conflict_is_fail_closed_and_preserves_external_files(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("trusted-reenroll-quarantine-target-conflict.db");
        seed_trusted_reenrollment_repository(
            &path,
            "trusted-reenroll-quarantine-target-conflict",
            "trusted.reenroll.quarantine.target.conflict",
        )
        .await;
        seed_trusted_reenrollment_sidecars(&path);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);

        let _token_guard = install_trusted_reenrollment_quarantine_token(path.clone(), "occupied");
        let quarantine = path_with_suffix(&path, ".quarantine-occupied");
        std::fs::write(&quarantine, b"external-db").unwrap();
        std::fs::write(
            path_with_suffix(&quarantine, "-journal"),
            b"external-journal",
        )
        .unwrap();
        std::fs::write(path_with_suffix(&quarantine, "-wal"), b"external-wal").unwrap();
        std::fs::write(path_with_suffix(&quarantine, "-shm"), b"external-shm").unwrap();

        let error = SqliteMcpPlatformRepository::trusted_reenroll_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RepositoryUnavailable);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);
        assert_file_contents(&quarantine, b"external-db");
        assert_file_contents(
            &path_with_suffix(&quarantine, "-journal"),
            b"external-journal",
        );
        assert_file_contents(&path_with_suffix(&quarantine, "-wal"), b"external-wal");
        assert_file_contents(&path_with_suffix(&quarantine, "-shm"), b"external-shm");

        let retry = SqliteMcpPlatformRepository::open_path(&path)
            .await
            .unwrap_err();
        assert_eq!(retry.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn trusted_reenrollment_quarantine_unlink_failure_preserves_external_replacement_and_original_database(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("trusted-reenroll-quarantine-unlink-failure.db");
        seed_trusted_reenrollment_repository(
            &path,
            "trusted-reenroll-quarantine-unlink-failure",
            "trusted.reenroll.quarantine.unlink.failure",
        )
        .await;
        seed_trusted_reenrollment_sidecars(&path);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);

        let _token_guard =
            install_trusted_reenrollment_quarantine_token(path.clone(), "unlink-failure");
        let quarantine = path_with_suffix(&path, ".quarantine-unlink-failure");
        let expected_source = path.clone();
        let _hook = install_move_file_source_unlink_failure_hook(path.clone(), {
            let quarantine = quarantine.clone();
            let expected_source = expected_source.clone();
            move |source, destination| {
                assert_eq!(source, expected_source.as_path());
                assert_eq!(destination, quarantine.as_path());
                std::fs::remove_file(destination).unwrap();
                std::fs::write(destination, b"external-db").unwrap();
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "managed MCP source unlink failed for testing",
                ))
            }
        });

        let error = SqliteMcpPlatformRepository::trusted_reenroll_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RepositoryUnavailable);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);
        assert_file_contents(&quarantine, b"external-db");
        assert!(!path_with_suffix(&quarantine, "-journal").exists());
        assert!(!path_with_suffix(&quarantine, "-wal").exists());
        assert!(!path_with_suffix(&quarantine, "-shm").exists());

        let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        reopened
            .get_managed_mcp("trusted-reenroll-quarantine-unlink-failure")
            .await
            .unwrap();
        reopened.close().await;
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn trusted_reenrollment_activation_target_conflict_is_fail_closed_and_preserves_external_files(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("trusted-reenroll-activation-target-conflict.db");
        seed_trusted_reenrollment_repository(
            &path,
            "trusted-reenroll-activation-target-conflict",
            "trusted.reenroll.activation.target.conflict",
        )
        .await;
        seed_trusted_reenrollment_sidecars(&path);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);

        let _hook = install_after_trusted_reenrollment_anchor_clear_hook(path.clone(), {
            let path = path.clone();
            move |database_path| {
                assert_eq!(database_path, path.as_path());
                std::fs::remove_file(database_path).unwrap();
                std::fs::write(database_path, b"external-db").unwrap();
                std::fs::write(
                    path_with_suffix(database_path, "-journal"),
                    b"external-journal",
                )
                .unwrap();
                std::fs::write(path_with_suffix(database_path, "-wal"), b"external-wal").unwrap();
                std::fs::write(path_with_suffix(database_path, "-shm"), b"external-shm").unwrap();
            }
        });

        let error = SqliteMcpPlatformRepository::trusted_reenroll_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RollbackIncomplete);
        assert_file_contents(&path, b"external-db");
        assert_file_contents(&path_with_suffix(&path, "-journal"), b"external-journal");
        assert_file_contents(&path_with_suffix(&path, "-wal"), b"external-wal");
        assert_file_contents(&path_with_suffix(&path, "-shm"), b"external-shm");

        let quarantine_prefix = "trusted-reenroll-activation-target-conflict.db.quarantine-";
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(quarantine_prefix)));
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(quarantine_prefix) && name.ends_with("-journal")));
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(quarantine_prefix) && name.ends_with("-wal")));
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(quarantine_prefix) && name.ends_with("-shm")));

        let retry = SqliteMcpPlatformRepository::open_path(&path)
            .await
            .unwrap_err();
        assert_eq!(retry.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn trusted_reenrollment_parent_binding_drift_after_lock_is_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let linked_parent = directory.path().join("linked-parent");
        let physical_parent_a = directory.path().join("physical-a");
        let physical_parent_b = directory.path().join("physical-b");
        std::fs::create_dir_all(&physical_parent_a).unwrap();
        std::fs::create_dir_all(&physical_parent_b).unwrap();
        create_parent_link(&linked_parent, &physical_parent_a);

        let path = linked_parent.join("trusted-reenroll-drift.db");
        let physical_path_a = physical_parent_a.join("trusted-reenroll-drift.db");
        let physical_path_b = physical_parent_b.join("trusted-reenroll-drift.db");
        let repository_a = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        repository_a
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "trusted-reenroll-drift-a".to_string(),
                    mcp_id: "trusted.reenroll.drift.a".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository_a.close().await;
        seed_trusted_reenrollment_sidecars(&physical_path_a);
        assert!(physical_path_a.exists());
        assert_trusted_reenrollment_sidecars_exist(&physical_path_a);

        let repository_b = SqliteMcpPlatformRepository::open_path(&physical_path_b)
            .await
            .unwrap();
        repository_b
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "trusted-reenroll-drift-b".to_string(),
                    mcp_id: "trusted.reenroll.drift.b".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository_b.close().await;
        seed_trusted_reenrollment_sidecars(&physical_path_b);
        assert!(physical_path_b.exists());
        assert_trusted_reenrollment_sidecars_exist(&physical_path_b);

        let lexical_lock_path = integrity_lock_path(&path);
        let actual_lock_path_a = integrity_lock_path(&physical_path_a);
        let _hook = install_after_database_lock_acquired_hook(lexical_lock_path.clone(), {
            let linked_parent = linked_parent.clone();
            let physical_parent_b = physical_parent_b.clone();
            let lexical_lock_path = lexical_lock_path.clone();
            let actual_lock_path_a = actual_lock_path_a.clone();
            move |acquired_path, resolved_path| {
                assert_eq!(acquired_path, lexical_lock_path.as_path());
                assert_eq!(resolved_path, actual_lock_path_a.canonicalize().unwrap());
                std::fs::remove_dir(&linked_parent).unwrap();
                create_parent_link(&linked_parent, &physical_parent_b);
            }
        });

        let error = SqliteMcpPlatformRepository::trusted_reenroll_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert!(physical_path_a.exists());
        assert!(physical_path_b.exists());
        assert_trusted_reenrollment_sidecars_exist(&physical_path_a);
        assert_trusted_reenrollment_sidecars_exist(&physical_path_b);
        assert!(actual_lock_path_a.exists());
        assert!(!std::fs::read_dir(&physical_parent_b)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with("trusted-reenroll-drift.db.quarantine-")));

        let reopened_a = SqliteMcpPlatformRepository::open_path(&physical_path_a)
            .await
            .unwrap();
        reopened_a
            .get_managed_mcp("trusted-reenroll-drift-a")
            .await
            .unwrap();
        reopened_a.close().await;

        let reopened_b = SqliteMcpPlatformRepository::open_path(&physical_path_b)
            .await
            .unwrap();
        reopened_b
            .get_managed_mcp("trusted-reenroll-drift-b")
            .await
            .unwrap();
        reopened_b.close().await;

        integrity::clear_system_anchor_for_trusted_reenrollment(
            &database_path_binding(&physical_path_a).unwrap(),
        )
        .unwrap();
        integrity::clear_system_anchor_for_trusted_reenrollment(
            &database_path_binding(&physical_path_b).unwrap(),
        )
        .unwrap();
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn trusted_reenrollment_after_anchor_clear_parent_drift_reopens_confirmed_physical_target(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let linked_parent = directory.path().join("linked-parent");
        let physical_parent_a = directory.path().join("physical-a");
        let physical_parent_b = directory.path().join("physical-b");
        std::fs::create_dir_all(&physical_parent_a).unwrap();
        std::fs::create_dir_all(&physical_parent_b).unwrap();
        create_parent_link(&linked_parent, &physical_parent_a);

        let path = linked_parent.join("trusted-reenroll-anchor-clear.db");
        let physical_path_a = physical_parent_a.join("trusted-reenroll-anchor-clear.db");
        let physical_path_b = physical_parent_b.join("trusted-reenroll-anchor-clear.db");
        let repository_a = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        repository_a
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "trusted-reenroll-anchor-clear-a".to_string(),
                    mcp_id: "trusted.reenroll.anchor.clear.a".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository_a.close().await;
        seed_trusted_reenrollment_sidecars(&physical_path_a);
        assert!(physical_path_a.exists());
        assert_trusted_reenrollment_sidecars_exist(&physical_path_a);

        let repository_b = SqliteMcpPlatformRepository::open_path(&physical_path_b)
            .await
            .unwrap();
        repository_b
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "trusted-reenroll-anchor-clear-b".to_string(),
                    mcp_id: "trusted.reenroll.anchor.clear.b".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository_b.close().await;
        seed_trusted_reenrollment_sidecars(&physical_path_b);
        assert!(physical_path_b.exists());
        assert_trusted_reenrollment_sidecars_exist(&physical_path_b);

        let _hook =
            install_after_trusted_reenrollment_anchor_clear_hook(physical_path_a.clone(), {
                let expected_database_path = physical_path_a.clone();
                let linked_parent = linked_parent.clone();
                let physical_parent_b = physical_parent_b.clone();
                move |database_path| {
                    assert_eq!(database_path, expected_database_path.as_path());
                    std::fs::remove_dir(&linked_parent).unwrap();
                    create_parent_link(&linked_parent, &physical_parent_b);
                }
            });

        let (repository, quarantine) = SqliteMcpPlatformRepository::trusted_reenroll_path(&path)
            .await
            .unwrap();
        assert_eq!(repository.database_path(), Some(physical_path_a.as_path()));
        assert_eq!(quarantine.parent(), Some(physical_parent_a.as_path()));
        assert!(quarantine.exists());
        assert_trusted_reenrollment_sidecars_absent(&physical_path_a);
        assert_trusted_reenrollment_sidecars_exist(&physical_path_b);
        assert_trusted_reenrollment_sidecars_exist(&quarantine);
        assert!(repository
            .get_managed_mcp("trusted-reenroll-anchor-clear-a")
            .await
            .is_err());
        assert!(repository
            .get_managed_mcp("trusted-reenroll-anchor-clear-b")
            .await
            .is_err());
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "trusted-reenroll-anchor-clear-new".to_string(),
                    mcp_id: "trusted.reenroll.anchor.clear.new".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository.close().await;

        let reopened_b = SqliteMcpPlatformRepository::open_path(&physical_path_b)
            .await
            .unwrap();
        reopened_b
            .get_managed_mcp("trusted-reenroll-anchor-clear-b")
            .await
            .unwrap();
        reopened_b.close().await;

        integrity::clear_system_anchor_for_trusted_reenrollment(
            &database_path_binding(&physical_path_a).unwrap(),
        )
        .unwrap();
        integrity::clear_system_anchor_for_trusted_reenrollment(
            &database_path_binding(&physical_path_b).unwrap(),
        )
        .unwrap();
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn trusted_reenrollment_parent_drift_with_target_conflict_does_not_overwrite_physical_b()
    {
        let directory = tempfile::tempdir().unwrap();
        let linked_parent = directory.path().join("linked-parent");
        let physical_parent_a = directory.path().join("physical-a");
        let physical_parent_b = directory.path().join("physical-b");
        std::fs::create_dir_all(&physical_parent_a).unwrap();
        std::fs::create_dir_all(&physical_parent_b).unwrap();
        create_parent_link(&linked_parent, &physical_parent_a);

        let path = linked_parent.join("trusted-reenroll-drift-conflict.db");
        let physical_path_a = physical_parent_a.join("trusted-reenroll-drift-conflict.db");
        let physical_path_b = physical_parent_b.join("trusted-reenroll-drift-conflict.db");
        let repository_a = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        repository_a
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "trusted-reenroll-drift-conflict-a".to_string(),
                    mcp_id: "trusted.reenroll.drift.conflict.a".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository_a.close().await;
        seed_trusted_reenrollment_sidecars(&physical_path_a);
        assert!(physical_path_a.exists());
        assert_trusted_reenrollment_sidecars_exist(&physical_path_a);

        let repository_b = SqliteMcpPlatformRepository::open_path(&physical_path_b)
            .await
            .unwrap();
        repository_b
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "trusted-reenroll-drift-conflict-b".to_string(),
                    mcp_id: "trusted.reenroll.drift.conflict.b".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository_b.close().await;
        seed_trusted_reenrollment_sidecars(&physical_path_b);
        assert!(physical_path_b.exists());
        assert_trusted_reenrollment_sidecars_exist(&physical_path_b);

        let _hook =
            install_after_trusted_reenrollment_anchor_clear_hook(physical_path_a.clone(), {
                let expected_database_path = physical_path_a.clone();
                let linked_parent = linked_parent.clone();
                let physical_parent_b = physical_parent_b.clone();
                move |database_path| {
                    assert_eq!(database_path, expected_database_path.as_path());
                    std::fs::remove_dir(&linked_parent).unwrap();
                    create_parent_link(&linked_parent, &physical_parent_b);
                    std::fs::write(database_path, b"external-db").unwrap();
                    std::fs::write(
                        path_with_suffix(database_path, "-journal"),
                        b"external-journal",
                    )
                    .unwrap();
                    std::fs::write(path_with_suffix(database_path, "-wal"), b"external-wal")
                        .unwrap();
                    std::fs::write(path_with_suffix(database_path, "-shm"), b"external-shm")
                        .unwrap();
                }
            });

        let error = SqliteMcpPlatformRepository::trusted_reenroll_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RollbackIncomplete);
        assert_file_contents(&physical_path_a, b"external-db");
        assert_file_contents(
            &path_with_suffix(&physical_path_a, "-journal"),
            b"external-journal",
        );
        assert_file_contents(&path_with_suffix(&physical_path_a, "-wal"), b"external-wal");
        assert_file_contents(&path_with_suffix(&physical_path_a, "-shm"), b"external-shm");
        assert!(!std::fs::read_dir(&physical_parent_b)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with("trusted-reenroll-drift-conflict.db.quarantine-")));

        let reopened_b = SqliteMcpPlatformRepository::open_path(&physical_path_b)
            .await
            .unwrap();
        reopened_b
            .get_managed_mcp("trusted-reenroll-drift-conflict-b")
            .await
            .unwrap();
        reopened_b.close().await;
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn trusted_reenrollment_sidecar_rename_failure_restores_original_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trusted-reenroll-sidecar-failure.db");
        let wal_path = path_with_suffix(&path, "-wal");
        let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "trusted-reenroll-sidecar-failure".to_string(),
                    mcp_id: "trusted.reenroll.sidecar.failure".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository.close().await;
        seed_trusted_reenrollment_sidecars(&path);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);

        let _guard =
            install_trusted_reenrollment_rename_failures(vec![TrustedReenrollmentRenameFailure {
                phase: TrustedReenrollmentRenamePhase::Move,
                source: Some(wal_path.clone()),
                destination: None,
            }]);

        let error = SqliteMcpPlatformRepository::trusted_reenroll_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RepositoryUnavailable);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);

        let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        reopened
            .get_managed_mcp("trusted-reenroll-sidecar-failure")
            .await
            .unwrap();
        reopened.close().await;
        integrity::clear_system_anchor_for_trusted_reenrollment(
            &database_path_binding(&path).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn database_lock_cleanup_uses_resolved_lock_path_after_parent_drift() {
        let directory = tempfile::tempdir().unwrap();
        let linked_parent = directory.path().join("linked-parent");
        let physical_parent_a = directory.path().join("physical-a");
        let physical_parent_b = directory.path().join("physical-b");
        std::fs::create_dir_all(&physical_parent_a).unwrap();
        std::fs::create_dir_all(&physical_parent_b).unwrap();
        create_parent_link(&linked_parent, &physical_parent_a);

        let logical_path = linked_parent.join("cleanup-drift.db");
        let lexical_lock_path = integrity_lock_path(&logical_path);
        let actual_lock_path_a = integrity_lock_path(&physical_parent_a.join("cleanup-drift.db"));
        let actual_lock_path_b = integrity_lock_path(&physical_parent_b.join("cleanup-drift.db"));
        let guard = DatabaseLockGuard::acquire(&lexical_lock_path).unwrap();
        assert_eq!(
            guard.resolved_path(),
            actual_lock_path_a.canonicalize().unwrap()
        );

        std::fs::remove_dir(&linked_parent).unwrap();
        create_parent_link(&linked_parent, &physical_parent_b);
        std::fs::write(&actual_lock_path_b, b"new-lock").unwrap();

        guard.cleanup().unwrap();

        assert!(actual_lock_path_a.exists());
        assert!(actual_lock_path_b.exists());
        assert_lock_sidecar_released(&actual_lock_path_a);
    }

    #[test]
    fn database_lock_cleanup_does_not_delete_reacquired_same_physical_lock_path() {
        let directory = tempfile::tempdir().unwrap();
        let lock_path = integrity_lock_path(&directory.path().join("cleanup-reacquire.db"));
        let guard = DatabaseLockGuard::acquire(&lock_path).unwrap();

        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let contender_lock_path = lock_path.clone();
        let contender = std::thread::spawn(move || {
            let file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .open(&contender_lock_path)
                .unwrap();
            file.lock_exclusive().unwrap();
            acquired_tx.send(()).unwrap();
            blocking_recv(release_rx);
            file.unlock().unwrap();
        });

        guard.cleanup().unwrap();
        blocking_recv(acquired_rx);
        assert!(lock_path.exists());

        let parallel_attempt = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        let error = parallel_attempt.try_lock_exclusive().unwrap_err();
        assert_lock_contention(error);
        drop(parallel_attempt);

        release_tx.send(()).unwrap();
        contender.join().unwrap();
        assert_lock_sidecar_released(&lock_path);
    }

    #[test]
    fn move_file_without_replace_same_path_is_a_noop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("same-path.db");
        std::fs::write(&path, b"same-path").unwrap();

        move_file_without_replace(&path, &path).unwrap();

        assert_file_contents(&path, b"same-path");
    }

    #[test]
    fn activation_source_unlink_failure_cleans_the_created_final_and_staged_paths() {
        let directory = tempfile::tempdir().unwrap();
        let staged = directory.path().join("staged.db");
        let final_path = directory.path().join("final.db");
        std::fs::write(&staged, b"staged-db").unwrap();
        std::fs::write(path_with_suffix(&staged, "-wal"), b"staged-wal").unwrap();
        std::fs::write(path_with_suffix(&final_path, "-shm"), b"final-shm").unwrap();
        let _hook = install_move_file_source_unlink_failure_hook(staged.clone(), |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "source unlink failed for testing",
            ))
        });

        let failure = activate_staged_database(&staged, &final_path).unwrap_err();

        assert!(failure.final_created);
        assert!(staged.exists());
        assert!(final_path.exists());
        let failure = FileBackedBootstrapFailure::created_final(failure.error, staged, final_path);
        let paths_to_cleanup = failure.cleanup_paths();
        cleanup_paths(&paths_to_cleanup, failure.cleanup_error()).unwrap();
        assert!(paths_to_cleanup.iter().all(|path| !path.exists()));
    }

    #[test]
    fn move_file_without_replace_preserves_existing_destination_and_moves_sidecars() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.db");
        let destination = directory.path().join("destination.db");
        std::fs::write(&source, b"source-db").unwrap();
        std::fs::write(&destination, b"destination-db").unwrap();

        let error = move_file_without_replace(&source, &destination).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_file_contents(&source, b"source-db");
        assert_file_contents(&destination, b"destination-db");

        std::fs::remove_file(&destination).unwrap();
        std::fs::write(path_with_suffix(&source, "-journal"), b"journal").unwrap();
        std::fs::write(path_with_suffix(&source, "-wal"), b"wal").unwrap();
        std::fs::write(path_with_suffix(&source, "-shm"), b"shm").unwrap();

        for (from, to) in sqlite_database_and_sidecar_paths(&source)
            .into_iter()
            .zip(sqlite_database_and_sidecar_paths(&destination))
        {
            move_file_without_replace(&from, &to).unwrap();
        }

        for path in sqlite_database_and_sidecar_paths(&source) {
            assert!(!path.exists());
        }
        assert_file_contents(&destination, b"source-db");
        assert_file_contents(&path_with_suffix(&destination, "-journal"), b"journal");
        assert_file_contents(&path_with_suffix(&destination, "-wal"), b"wal");
        assert_file_contents(&path_with_suffix(&destination, "-shm"), b"shm");
    }

    #[test]
    fn test_injection_state_is_path_scoped_and_drop_cleans_up() {
        let directory = tempfile::tempdir().unwrap();
        let path_a = directory.path().join("a.db");
        let path_b = directory.path().join("b.db");
        let staged_a = path_with_suffix(&path_a, ".stage");
        let staged_b = path_with_suffix(&path_b, ".stage");
        let hook_hits_a = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hook_hits_b = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let preflight_a = install_after_preflight_before_lock_hook(path_a.clone(), {
            let hook_hits_a = hook_hits_a.clone();
            move |_| {
                hook_hits_a.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        let preflight_b = install_after_preflight_before_lock_hook(path_b.clone(), {
            let hook_hits_b = hook_hits_b.clone();
            move |_| {
                hook_hits_b.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        run_after_preflight_before_lock_hook(&path_a);
        assert_eq!(hook_hits_a.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(hook_hits_b.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(preflight_b);
        run_after_preflight_before_lock_hook(&path_b);
        assert_eq!(hook_hits_b.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(preflight_a);

        let lock_a = integrity_lock_path(&path_a);
        let lock_b = integrity_lock_path(&path_b);
        let lock_hook_a = install_after_database_lock_acquired_hook(lock_a.clone(), {
            let hook_hits_a = hook_hits_a.clone();
            move |_, _| {
                hook_hits_a.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        let lock_hook_b = install_after_database_lock_acquired_hook(lock_b.clone(), {
            let hook_hits_b = hook_hits_b.clone();
            move |_, _| {
                hook_hits_b.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        run_after_database_lock_acquired_hook(&lock_a, &lock_a);
        assert_eq!(hook_hits_a.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(hook_hits_b.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(lock_hook_b);
        run_after_database_lock_acquired_hook(&lock_b, &lock_b);
        assert_eq!(hook_hits_b.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(lock_hook_a);

        let activation_a = install_after_activation_before_final_open_hook(path_a.clone(), {
            let hook_hits_a = hook_hits_a.clone();
            move |_, _| {
                hook_hits_a.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        });
        let activation_b = install_after_activation_before_final_open_hook(path_b.clone(), {
            let hook_hits_b = hook_hits_b.clone();
            move |_, _| {
                hook_hits_b.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        });
        run_after_activation_before_final_open_hook(&path_a, &staged_a).unwrap();
        assert_eq!(hook_hits_a.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert_eq!(hook_hits_b.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(activation_b);
        run_after_activation_before_final_open_hook(&path_b, &staged_b).unwrap();
        assert_eq!(hook_hits_b.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(activation_a);

        let anchor_clear_a =
            install_after_trusted_reenrollment_anchor_clear_hook(path_a.clone(), {
                let hook_hits_a = hook_hits_a.clone();
                move |_| {
                    hook_hits_a.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            });
        let anchor_clear_b =
            install_after_trusted_reenrollment_anchor_clear_hook(path_b.clone(), {
                let hook_hits_b = hook_hits_b.clone();
                move |_| {
                    hook_hits_b.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            });
        run_after_trusted_reenrollment_anchor_clear_hook(&path_a);
        assert_eq!(hook_hits_a.load(std::sync::atomic::Ordering::SeqCst), 4);
        assert_eq!(hook_hits_b.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(anchor_clear_b);
        run_after_trusted_reenrollment_anchor_clear_hook(&path_b);
        assert_eq!(hook_hits_b.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(anchor_clear_a);

        let rename_guard_a =
            install_trusted_reenrollment_rename_failures(vec![TrustedReenrollmentRenameFailure {
                phase: TrustedReenrollmentRenamePhase::Move,
                source: Some(path_a.clone()),
                destination: Some(staged_a.clone()),
            }]);
        let rename_guard_b =
            install_trusted_reenrollment_rename_failures(vec![TrustedReenrollmentRenameFailure {
                phase: TrustedReenrollmentRenamePhase::Move,
                source: Some(path_b.clone()),
                destination: Some(staged_b.clone()),
            }]);
        assert!(maybe_fail_trusted_reenrollment_rename(
            &path_a,
            &staged_a,
            TrustedReenrollmentRenamePhase::Move
        )
        .is_err());
        drop(rename_guard_b);
        assert!(maybe_fail_trusted_reenrollment_rename(
            &path_b,
            &staged_b,
            TrustedReenrollmentRenamePhase::Move
        )
        .is_ok());
        drop(rename_guard_a);
    }

    #[test]
    fn test_injection_state_preserves_same_path_registration_order_and_isolation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("same-path.db");
        let staged = path_with_suffix(&path, ".stage");
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));

        let preflight_first = install_after_preflight_before_lock_hook(path.clone(), {
            let events = events.clone();
            move |_| events.lock().unwrap().push("preflight-first")
        });
        let preflight_second = install_after_preflight_before_lock_hook(path.clone(), {
            let events = events.clone();
            move |_| events.lock().unwrap().push("preflight-second")
        });
        run_after_preflight_before_lock_hook(&path);
        run_after_preflight_before_lock_hook(&path);
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["preflight-first", "preflight-second"]
        );
        drop(preflight_first);
        drop(preflight_second);

        let preflight_dropped = install_after_preflight_before_lock_hook(path.clone(), {
            let events = events.clone();
            move |_| events.lock().unwrap().push("preflight-dropped")
        });
        let preflight_kept = install_after_preflight_before_lock_hook(path.clone(), {
            let events = events.clone();
            move |_| events.lock().unwrap().push("preflight-kept")
        });
        drop(preflight_dropped);
        run_after_preflight_before_lock_hook(&path);
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["preflight-first", "preflight-second", "preflight-kept"]
        );
        drop(preflight_kept);

        let rename_first =
            install_trusted_reenrollment_rename_failures(vec![TrustedReenrollmentRenameFailure {
                phase: TrustedReenrollmentRenamePhase::Move,
                source: Some(path.clone()),
                destination: Some(staged.clone()),
            }]);
        let rename_second =
            install_trusted_reenrollment_rename_failures(vec![TrustedReenrollmentRenameFailure {
                phase: TrustedReenrollmentRenamePhase::Move,
                source: Some(path.clone()),
                destination: Some(staged.clone()),
            }]);
        assert!(maybe_fail_trusted_reenrollment_rename(
            &path,
            &staged,
            TrustedReenrollmentRenamePhase::Move
        )
        .is_err());
        assert!(maybe_fail_trusted_reenrollment_rename(
            &path,
            &staged,
            TrustedReenrollmentRenamePhase::Move
        )
        .is_err());
        drop(rename_first);
        drop(rename_second);

        let rename_dropped =
            install_trusted_reenrollment_rename_failures(vec![TrustedReenrollmentRenameFailure {
                phase: TrustedReenrollmentRenamePhase::Move,
                source: Some(path.clone()),
                destination: Some(staged.clone()),
            }]);
        let rename_kept =
            install_trusted_reenrollment_rename_failures(vec![TrustedReenrollmentRenameFailure {
                phase: TrustedReenrollmentRenamePhase::Move,
                source: Some(path.clone()),
                destination: Some(staged.clone()),
            }]);
        drop(rename_dropped);
        assert!(maybe_fail_trusted_reenrollment_rename(
            &path,
            &staged,
            TrustedReenrollmentRenamePhase::Move
        )
        .is_err());
        assert!(maybe_fail_trusted_reenrollment_rename(
            &path,
            &staged,
            TrustedReenrollmentRenamePhase::Move
        )
        .is_ok());
        drop(rename_kept);
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn trusted_reenrollment_restore_failure_returns_rollback_incomplete() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trusted-reenroll-restore-failure.db");
        let wal_path = path_with_suffix(&path, "-wal");
        let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        repository.close().await;
        seed_trusted_reenrollment_sidecars(&path);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);

        let _guard = install_trusted_reenrollment_rename_failures(vec![
            TrustedReenrollmentRenameFailure {
                phase: TrustedReenrollmentRenamePhase::Move,
                source: Some(wal_path.clone()),
                destination: None,
            },
            TrustedReenrollmentRenameFailure {
                phase: TrustedReenrollmentRenamePhase::Restore,
                source: None,
                destination: Some(path.clone()),
            },
        ]);

        let error = SqliteMcpPlatformRepository::trusted_reenroll_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RollbackIncomplete);
        assert!(!path.exists());
        assert!(!path_with_suffix(&path, "-journal").exists());
        assert!(wal_path.exists());
        assert!(!path_with_suffix(&path, "-shm").exists());
        let quarantine_prefix = "trusted-reenroll-restore-failure.db.quarantine-";
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(quarantine_prefix) && name.ends_with("-journal")));
        assert!(std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(quarantine_prefix) && name.ends_with("-shm")));
        let retry = SqliteMcpPlatformRepository::open_path(&path)
            .await
            .unwrap_err();
        assert_eq!(retry.code(), McpPlatformErrorCode::IntegrityError);
        integrity::clear_system_anchor_for_trusted_reenrollment(
            &database_path_binding(&path).unwrap(),
        )
        .unwrap();
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn trusted_reenrollment_anchor_clear_failure_restores_original_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trusted-reenroll-rollback.db");
        let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "trusted-reenroll-rollback".to_string(),
                    mcp_id: "trusted.reenroll.rollback".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository.close().await;
        seed_trusted_reenrollment_sidecars(&path);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);

        let _guard = integrity::fail_next_clear_system_anchor_for_trusted_reenrollment(
            database_path_binding(&path).unwrap(),
        );
        let error = SqliteMcpPlatformRepository::trusted_reenroll_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert!(path.exists());
        assert_trusted_reenrollment_sidecars_exist(&path);
        let quarantine_prefix = format!(
            "{}.quarantine-",
            path.file_name().unwrap().to_string_lossy()
        );
        assert!(!std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with(&quarantine_prefix)));

        let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        reopened
            .get_managed_mcp("trusted-reenroll-rollback")
            .await
            .unwrap();
        reopened.close().await;
        integrity::clear_system_anchor_for_trusted_reenrollment(
            &database_path_binding(&path).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn pathless_url_open_fails_closed_before_repository_bootstrap() {
        let error = SqliteMcpPlatformRepository::open_url("sqlite::memory:")
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
    }

    #[tokio::test]
    async fn whole_database_snapshot_replay_is_rejected_by_external_sequence() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snapshot-replay.db");
        let snapshot = directory.path().join("snapshot-replay.old");
        let signer = InMemoryIntegritySigner::new_for_testing([0x31; 32]);
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "snapshot-managed".to_string(),
                    mcp_id: "snapshot.example".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        repository.close().await;
        std::fs::copy(&path, &snapshot).unwrap();

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let second_managed = repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "snapshot-managed-second".to_string(),
                    mcp_id: "snapshot.second.example".to_string(),
                    installation_scope: "user".to_string(),
                },
                2,
            )
            .await
            .unwrap();
        assert_eq!(second_managed.mcp_id, "snapshot.second.example");
        repository.close().await;
        std::fs::copy(&snapshot, &path).unwrap();

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[tokio::test]
    async fn copied_database_is_rejected_at_a_different_bound_path() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("bound-source.db");
        let copied = directory.path().join("bound-copy.db");
        let source_binding = database_path_binding(&source).unwrap();
        let signer =
            InMemoryIntegritySigner::new_for_testing_with_path_binding([0x32; 32], source_binding);
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&source, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        std::fs::copy(&source, &copied).unwrap();

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&copied, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[cfg(unix)]
    #[test]
    fn path_binding_preserves_case_and_non_utf8_bytes_on_case_sensitive_platforms() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let directory = tempfile::tempdir().unwrap();
        let upper = directory.path().join("Platform.db");
        let lower = directory.path().join("platform.db");
        assert_ne!(
            database_path_binding(&upper).unwrap(),
            database_path_binding(&lower).unwrap()
        );

        let first = directory
            .path()
            .join(OsString::from_vec(b"platform-\x80.db".to_vec()));
        let second = directory
            .path()
            .join(OsString::from_vec(b"platform-\x81.db".to_vec()));
        assert_ne!(
            database_path_binding(&first).unwrap(),
            database_path_binding(&second).unwrap()
        );
    }

    #[cfg(windows)]
    #[test]
    fn path_binding_does_not_collapse_case_distinct_lexical_paths() {
        let directory = tempfile::tempdir().unwrap();
        assert_ne!(
            database_path_binding(&directory.path().join("Platform.db")).unwrap(),
            database_path_binding(&directory.path().join("platform.db")).unwrap()
        );
    }

    #[tokio::test]
    async fn fresh_file_repository_records_projection_witness_v2_schema_at_v15() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fresh-v15.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x30; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        repository.close().await;

        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(
            sqlite_table_columns(&path, "projection_mutations").await,
            vec![
                "mutation_id",
                "managed_mcp_id",
                "expected_revision",
                "previous_enabled",
                "desired_enabled",
                "status",
                "writer_runtime_id",
                "writer_sink_id",
                "writer_anchor_instance_id",
                "writer_anchor_path_binding",
                "writer_anchor_key_epoch",
                "writer_anchor_sequence",
                "writer_anchor_root",
                "writer_witness_version",
                "writer_provider_id",
                "writer_binding_domain",
                "writer_observed_state_digest",
                "writer_commitment",
                "created_at_ms",
                "updated_at_ms"
            ]
        );
    }

    #[tokio::test]
    async fn fresh_file_repository_records_managed_enrollment_authority_evidence_schema_at_v18() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fresh-v18.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x31; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        repository.close().await;

        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert!(
            sqlite_table_has_column(
                &path,
                "managed_credential_enrollments",
                "authority_evidence_digest"
            )
            .await
        );
    }

    #[tokio::test]
    async fn v31_profile_schema_upgrades_once_to_hardened_lifecycle_impacts_v33() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v31-profile-lifecycle-impacts.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x6f; 32],
            database_path_binding(&path).unwrap(),
        );

        create_repository_fixture_with_version(&path, signer.clone(), 31).await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1_i64..=31).collect::<Vec<_>>()
        );
        assert!(
            !sqlite_schema_object_exists(&path, "table", "mcp_profile_lifecycle_impacts").await
        );

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;

        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert!(sqlite_schema_object_exists(&path, "table", "mcp_profile_lifecycle_impacts").await);
        assert_eq!(
            sqlite_table_columns(&path, "mcp_profile_lifecycle_impacts").await,
            vec![
                "impact_digest",
                "plan_id",
                "plan_digest",
                "managed_mcp_id",
                "actor_id",
                "operation",
                "expires_at_ms",
                "managed_revision",
                "manifest_evidence_digest",
                "projection_evidence_digest",
                "state",
                "confirmed_at_ms",
                "consumed_at_ms",
                "created_at_ms",
                "updated_at_ms",
            ]
        );
        assert!(
            !sqlite_table_has_column(
                &path,
                "mcp_profile_lifecycle_impacts",
                "impact_snapshot_json"
            )
            .await
        );
        let indexes = sqlite_table_indexes(&path, "mcp_profile_lifecycle_impacts").await;
        assert!(indexes.contains(&"mcp_profile_lifecycle_impacts_plan_id".to_string()));
        assert!(indexes.contains(&"mcp_profile_lifecycle_impacts_managed_mcp_id".to_string()));
        assert!(indexes.contains(&"mcp_profile_lifecycle_impacts_expires_at_ms".to_string()));
        assert!(
            sqlite_schema_object_exists(
                &path,
                "trigger",
                "mcp_profile_lifecycle_impacts_initial_state_is_planned"
            )
            .await
        );
        assert!(
            sqlite_schema_object_exists(
                &path,
                "trigger",
                "mcp_profile_lifecycle_impacts_state_transition_is_monotonic"
            )
            .await
        );
        assert!(
            sqlite_schema_object_exists(
                &path,
                "trigger",
                "mcp_profile_lifecycle_impacts_updated_at_ms_is_monotonic"
            )
            .await
        );

        let schema_versions_after_upgrade = sqlite_schema_versions(&path).await;
        let commits_after_upgrade = sqlite_integrity_commit_count(&path).await;
        let reopened = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap();
        reopened.close().await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            schema_versions_after_upgrade
        );
        assert_eq!(
            sqlite_integrity_commit_count(&path).await,
            commits_after_upgrade
        );

        let pool = existing_repository_pool(&path).await;
        let plan_digest = "a".repeat(64);
        let expires_at_ms = profile_lifecycle_test_expiry_ms();
        sqlx::query(
            "INSERT INTO mcp_profiles(profile_id,name,description,revision,archived,created_at_ms,updated_at_ms,credential_references_json) VALUES ('profile-v32','Profile','fixture',1,0,1,1,'[]')",
        )
        .execute(&pool)
        .await
        .unwrap();
        for (index, managed_mcp_id) in [
            "managed-v32",
            "managed-v32-invalid-state",
            "managed-v32-invalid-operation",
            "managed-v32-confirmed-before-created",
            "managed-v32-consumed-before-confirmed",
            "managed-v32-confirmed-after-expiry",
            "managed-v32-consumed-after-expiry",
            "managed-v32-raw-actor",
            "managed-v32-parent-expiry",
        ]
        .into_iter()
        .enumerate()
        {
            sqlx::query(
                "INSERT INTO managed_mcps(managed_mcp_id,mcp_id,installation_scope,state_json,revision,created_at_ms,updated_at_ms) VALUES (?,?,'user','{}',1,1,1)",
            )
            .bind(managed_mcp_id)
            .bind(format!("mcp-v32-{index}"))
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO mcp_profile_apply_plans(plan_id,profile_id,profile_revision,plan_digest,snapshot_json,actor,expires_at_ms,idempotency_key,request_digest,created_at_ms) VALUES ('plan-v32','profile-v32',1,?,'{}','actor-v32',?,'plan-key-v32',?,1)",
        )
        .bind(&plan_digest)
        .bind(expires_at_ms)
        .bind("b".repeat(64))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'plan-v32',?,'managed-v32','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','archive',?,1,?,?,'planned',NULL,NULL,1,1)",
        )
        .bind("c".repeat(64))
        .bind(&plan_digest)
        .bind(expires_at_ms)
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .execute(&pool)
        .await
        .unwrap();
        let duplicate_plan_target = sqlx::query(
            "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'plan-v32',?,'managed-v32','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','archive',10,1,?,?,'planned',NULL,NULL,1,1)",
        )
        .bind("f".repeat(64))
        .bind(&plan_digest)
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .execute(&pool)
        .await;
        assert!(duplicate_plan_target.is_err());
        let snapshot_column_insert = sqlx::query(
            "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,impact_snapshot_json,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'plan-v32',?,'managed-v32-invalid-state','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','archive',10,1,?,?,'arbitrary-snapshot','planned',NULL,NULL,1,1)",
        )
        .bind("f".repeat(64))
        .bind(&plan_digest)
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .execute(&pool)
        .await;
        assert!(snapshot_column_insert.is_err());

        for (impact_digest_digit, managed_mcp_id) in [
            ("f", "managed-v32-invalid-state"),
            ("0", "managed-v32-confirmed-before-created"),
            ("1", "managed-v32-consumed-before-confirmed"),
            ("2", "managed-v32-confirmed-after-expiry"),
            ("3", "managed-v32-consumed-after-expiry"),
        ] {
            let impact_digest = impact_digest_digit.repeat(64);
            insert_v32_planned_lifecycle_impact(
                &pool,
                &impact_digest,
                &plan_digest,
                managed_mcp_id,
            )
            .await;
        }

        let invalid_state = sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET state='unknown' WHERE managed_mcp_id='managed-v32-invalid-state'",
        )
        .execute(&pool)
        .await;
        assert!(invalid_state.is_err());

        let invalid_operation = sqlx::query(
            "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'plan-v32',?,'managed-v32-invalid-operation','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','unknown',10,1,?,?,'planned',NULL,NULL,1,1)",
        )
        .bind("f".repeat(64))
        .bind(&plan_digest)
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .execute(&pool)
        .await;
        assert!(invalid_operation.is_err());

        let confirmed_before_created = sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET state='confirmed',confirmed_at_ms=0,updated_at_ms=1 WHERE managed_mcp_id='managed-v32-confirmed-before-created'",
        )
        .execute(&pool)
        .await;
        assert!(confirmed_before_created.is_err());

        sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET state='confirmed',confirmed_at_ms=5,updated_at_ms=5 WHERE managed_mcp_id='managed-v32-consumed-before-confirmed'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let consumed_before_confirmed = sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET state='consumed',consumed_at_ms=4 WHERE managed_mcp_id='managed-v32-consumed-before-confirmed'",
        )
        .execute(&pool)
        .await;
        assert!(consumed_before_confirmed.is_err());

        let confirmed_after_expiry = sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET state='confirmed',confirmed_at_ms=11,updated_at_ms=11 WHERE managed_mcp_id='managed-v32-confirmed-after-expiry'",
        )
        .execute(&pool)
        .await;
        assert!(confirmed_after_expiry.is_err());

        sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET state='confirmed',confirmed_at_ms=5,updated_at_ms=5 WHERE managed_mcp_id='managed-v32-consumed-after-expiry'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let consumed_after_expiry = sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET state='consumed',consumed_at_ms=11,updated_at_ms=11 WHERE managed_mcp_id='managed-v32-consumed-after-expiry'",
        )
        .execute(&pool)
        .await;
        assert!(consumed_after_expiry.is_err());

        sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET state='confirmed',confirmed_at_ms=2,updated_at_ms=2 WHERE managed_mcp_id='managed-v32'",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET state='consumed',consumed_at_ms=3,updated_at_ms=3 WHERE managed_mcp_id='managed-v32'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let state_rollback = sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET state='planned',confirmed_at_ms=NULL,consumed_at_ms=NULL,updated_at_ms=4 WHERE managed_mcp_id='managed-v32'",
        )
        .execute(&pool)
        .await;
        assert!(state_rollback.is_err());
        let updated_at_regression = sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET updated_at_ms=2 WHERE managed_mcp_id='managed-v32'",
        )
        .execute(&pool)
        .await;
        assert!(updated_at_regression.is_err());
        let confirmation_rewrite = sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET confirmed_at_ms=1,updated_at_ms=4 WHERE managed_mcp_id='managed-v32'",
        )
        .execute(&pool)
        .await;
        assert!(confirmation_rewrite.is_err());
        let consumption_rewrite = sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET consumed_at_ms=4,updated_at_ms=4 WHERE managed_mcp_id='managed-v32'",
        )
        .execute(&pool)
        .await;
        assert!(consumption_rewrite.is_err());

        let missing_parent = sqlx::query(
            "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'missing-plan',?,'managed-v32-invalid-state','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','archive',10,1,?,?,'planned',NULL,NULL,1,1)",
        )
        .bind("4".repeat(64))
        .bind(&plan_digest)
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .execute(&pool)
        .await;
        assert!(missing_parent.is_err());
        let missing_managed_mcp = sqlx::query(
            "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'plan-v32',?,'missing-managed-mcp','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','archive',10,1,?,?,'planned',NULL,NULL,1,1)",
        )
        .bind("5".repeat(64))
        .bind(&plan_digest)
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .execute(&pool)
        .await;
        assert!(missing_managed_mcp.is_err());

        let raw_actor = sqlx::query(
            "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'plan-v32',?,'managed-v32-raw-actor','credential=secret','archive',10,1,?,?,'planned',NULL,NULL,1,1)",
        )
        .bind("6".repeat(64))
        .bind(&plan_digest)
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .execute(&pool)
        .await;
        assert!(raw_actor.is_err());
        let expiry_rewrite = sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET expires_at_ms=11 WHERE managed_mcp_id='managed-v32'",
        )
        .execute(&pool)
        .await;
        assert!(expiry_rewrite.is_err());
        let created_at_rewrite = sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET created_at_ms=2 WHERE managed_mcp_id='managed-v32'",
        )
        .execute(&pool)
        .await;
        assert!(created_at_rewrite.is_err());
        let actor_rewrite = sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET actor_id=? WHERE managed_mcp_id='managed-v32'",
        )
        .bind("b".repeat(64))
        .execute(&pool)
        .await;
        assert!(actor_rewrite.is_err());
        sqlx::query(
            "INSERT INTO mcp_profile_apply_plans(plan_id,profile_id,profile_revision,plan_digest,snapshot_json,actor,expires_at_ms,idempotency_key,request_digest,created_at_ms) VALUES ('plan-v32-rebind','profile-v32',1,?,'{}','actor-v32',10,'plan-key-v32-rebind',?,1)",
        )
        .bind(&plan_digest)
        .bind("b".repeat(64))
        .execute(&pool)
        .await
        .unwrap();
        for statement in [
            "UPDATE mcp_profile_lifecycle_impacts SET plan_id='plan-v32-rebind' WHERE managed_mcp_id='managed-v32'",
            "UPDATE mcp_profile_lifecycle_impacts SET plan_digest='b' WHERE managed_mcp_id='managed-v32'",
            "UPDATE mcp_profile_lifecycle_impacts SET managed_mcp_id='managed-v32-invalid-state' WHERE managed_mcp_id='managed-v32'",
            "UPDATE mcp_profile_lifecycle_impacts SET operation='unarchive' WHERE managed_mcp_id='managed-v32'",
            "UPDATE mcp_profile_apply_plans SET plan_digest='b' WHERE plan_id='plan-v32'",
        ] {
            assert!(sqlx::query(statement).execute(&pool).await.is_err());
        }
        let parent_digest_mismatch = sqlx::query(
            "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'plan-v32',?,'managed-v32-invalid-state','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','archive',10,1,?,?,'planned',NULL,NULL,1,1)",
        )
        .bind("9".repeat(64))
        .bind("b".repeat(64))
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .execute(&pool)
        .await;
        assert!(parent_digest_mismatch.is_err());
        let blob_actor = sqlx::query(
            "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'plan-v32',?,'managed-v32-raw-actor',zeroblob(64),'archive',10,1,?,?,'planned',NULL,NULL,1,1)",
        )
        .bind("9".repeat(64))
        .bind(&plan_digest)
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .execute(&pool)
        .await;
        assert!(blob_actor.is_err());
        assert!(sqlx::query("INSERT INTO mcp_profile_lifecycle_impact_migration_audits(impact_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) VALUES (zeroblob(64),? ,?,'sanitized',0)")
            .bind("a".repeat(64))
            .bind("b".repeat(64))
            .execute(&pool)
            .await
            .is_err());
        let expiry_beyond_parent = sqlx::query(
            "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'plan-v32',?,'managed-v32-parent-expiry',?,'archive',11,1,?,?,'planned',NULL,NULL,1,1)",
        )
        .bind("7".repeat(64))
        .bind(&plan_digest)
        .bind("a".repeat(64))
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .execute(&pool)
        .await;
        assert!(expiry_beyond_parent.is_err());

        insert_v32_planned_lifecycle_impact(
            &pool,
            &"8".repeat(64),
            &plan_digest,
            "managed-v32-parent-expiry",
        )
        .await;
        let parent_expiry_rewrite = sqlx::query(
            "UPDATE mcp_profile_apply_plans SET expires_at_ms=4 WHERE plan_id='plan-v32'",
        )
        .execute(&pool)
        .await;
        assert!(parent_expiry_rewrite.is_err());
        sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET state='confirmed',confirmed_at_ms=5,updated_at_ms=5 WHERE managed_mcp_id='managed-v32-parent-expiry'",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE mcp_profile_lifecycle_impacts SET state='consumed',consumed_at_ms=6,updated_at_ms=6 WHERE managed_mcp_id='managed-v32-parent-expiry'",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn v32_lifecycle_impact_upgrade_sanitizes_legacy_actor_without_retaining_raw_value() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v32-profile-lifecycle-impacts.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x70; 32],
            database_path_binding(&path).unwrap(),
        );
        create_repository_fixture_with_version(&path, signer.clone(), 32).await;
        let pool = existing_repository_pool(&path).await;
        let plan_digest = "a".repeat(64);
        let expires_at_ms = profile_lifecycle_test_expiry_ms();
        let raw_actor = "credential=legacy-secret";
        sqlx::query(
            "INSERT INTO mcp_profiles(profile_id,name,description,revision,archived,created_at_ms,updated_at_ms,credential_references_json) VALUES ('profile-v33','Profile','fixture',1,0,1,1,'[]')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO managed_mcps(managed_mcp_id,mcp_id,installation_scope,state_json,revision,created_at_ms,updated_at_ms) VALUES ('managed-v33','mcp-v33','user','{}',1,1,1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mcp_profile_apply_plans(plan_id,profile_id,profile_revision,plan_digest,snapshot_json,actor,expires_at_ms,idempotency_key,request_digest,created_at_ms) VALUES ('plan-v33','profile-v33',1,?,'{}','actor-v33',?,'plan-key-v33',?,1)",
        )
        .bind(&plan_digest)
        .bind(expires_at_ms)
        .bind("b".repeat(64))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'plan-v33',?,'managed-v33',?,'archive',?,1,?,?,'planned',NULL,NULL,1,1)",
        )
        .bind("c".repeat(64))
        .bind(&plan_digest)
        .bind(raw_actor)
        .bind(expires_at_ms)
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        repository.close().await;

        let pool = existing_repository_pool(&path).await;
        let actor_id = sqlx::query_scalar::<_, String>(
            "SELECT actor_id FROM mcp_profile_lifecycle_impacts WHERE impact_digest=?",
        )
        .bind("c".repeat(64))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_ne!(actor_id, raw_actor);
        assert_eq!(actor_id.len(), 64);
        assert!(actor_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let audit = sqlx::query_as::<_, (String, String)>(
            "SELECT status,sanitized_actor_id FROM mcp_profile_lifecycle_impact_migration_audits WHERE impact_digest=?",
        )
        .bind("c".repeat(64))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audit.0, "sanitized");
        assert_eq!(audit.1, actor_id);
        let raw_rows = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM mcp_profile_lifecycle_impacts WHERE actor_id=?",
        )
        .bind(raw_actor)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(raw_rows, 0);
        pool.close().await;
    }

    #[tokio::test]
    async fn v35_lifecycle_impacts_reject_replace_delete_and_expired_authorizations() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v35-profile-lifecycle-impacts.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x71; 32],
            database_path_binding(&path).unwrap(),
        );
        create_repository_fixture_with_version(&path, signer.clone(), 34).await;
        let pool = existing_repository_pool(&path).await;
        let plan_digest = "a".repeat(64);
        let future = profile_lifecycle_test_expiry_ms();
        for managed_mcp_id in ["managed-v35", "managed-v35-expired"] {
            sqlx::query("INSERT INTO managed_mcps(managed_mcp_id,mcp_id,installation_scope,state_json,revision,created_at_ms,updated_at_ms) VALUES (?,?,'user','{}',1,1,1)")
                .bind(managed_mcp_id)
                .bind(format!("mcp-{managed_mcp_id}"))
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO mcp_profiles(profile_id,name,description,revision,archived,created_at_ms,updated_at_ms,credential_references_json) VALUES ('profile-v35','Profile','fixture',1,0,1,1,'[]')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO mcp_profile_apply_plans(plan_id,profile_id,profile_revision,plan_digest,snapshot_json,actor,expires_at_ms,idempotency_key,request_digest,created_at_ms) VALUES ('plan-v35','profile-v35',1,?,'{}','actor-v35',?,'plan-key-v35',?,1)")
            .bind(&plan_digest)
            .bind(future)
            .bind("b".repeat(64))
            .execute(&pool)
            .await
            .unwrap();
        for (impact_digest, managed_mcp_id, expires_at_ms) in [
            ("c".repeat(64), "managed-v35", future),
            ("f".repeat(64), "managed-v35-expired", 1_i64),
        ] {
            sqlx::query("INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'plan-v35',?,?,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','archive',?,1,?,?,'planned',NULL,NULL,1,1)")
                .bind(impact_digest)
                .bind(&plan_digest)
                .bind(managed_mcp_id)
                .bind(expires_at_ms)
                .bind("d".repeat(64))
                .bind("e".repeat(64))
                .execute(&pool)
                .await
                .unwrap();
        }
        pool.close().await;

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        repository.close().await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );

        let pool = existing_repository_pool(&path).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mcp_profile_lifecycle_impacts")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM mcp_profile_lifecycle_impact_v35_migration_audits"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "discarded_expired"
        );
        for recursive_triggers in ["OFF", "ON"] {
            sqlx::query(&format!("PRAGMA recursive_triggers={recursive_triggers}"))
                .execute(&pool)
                .await
                .unwrap();
            assert!(
                sqlx::query("DELETE FROM mcp_profile_lifecycle_impacts WHERE impact_digest=?")
                    .bind("c".repeat(64))
                    .execute(&pool)
                    .await
                    .is_err()
            );
            assert!(sqlx::query("INSERT OR REPLACE INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'plan-v35',?,'managed-v35','bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','archive',?,1,?,?,'planned',NULL,NULL,1,1)")
                .bind("c".repeat(64))
                .bind(&plan_digest)
                .bind(profile_lifecycle_test_expiry_ms())
                .bind("d".repeat(64))
                .bind("e".repeat(64))
                .execute(&pool)
                .await
                .is_err());
            assert!(
                sqlx::query("DELETE FROM mcp_profile_apply_plans WHERE plan_id='plan-v35'")
                    .execute(&pool)
                    .await
                    .is_err()
            );
            assert!(sqlx::query("INSERT OR REPLACE INTO mcp_profile_apply_plans(plan_id,profile_id,profile_revision,plan_digest,snapshot_json,actor,expires_at_ms,idempotency_key,request_digest,created_at_ms) VALUES ('plan-v35','profile-v35',1,?,'{}','replacement',?,'plan-key-v35',?,1)")
                .bind(&plan_digest)
                .bind(profile_lifecycle_test_expiry_ms())
                .bind("b".repeat(64))
                .execute(&pool)
                .await
                .is_err());
        }
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT actor_id FROM mcp_profile_lifecycle_impacts WHERE impact_digest=?"
            )
            .bind("c".repeat(64))
            .fetch_one(&pool)
            .await
            .unwrap(),
            "a".repeat(64)
        );
        sqlx::query("UPDATE mcp_profile_lifecycle_impacts SET state='confirmed',confirmed_at_ms=2,updated_at_ms=2 WHERE impact_digest=?")
            .bind("c".repeat(64))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE mcp_profile_lifecycle_impacts SET state='consumed',consumed_at_ms=3,updated_at_ms=3 WHERE impact_digest=?")
            .bind("c".repeat(64))
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT state FROM mcp_profile_lifecycle_impacts WHERE impact_digest=?"
            )
            .bind("c".repeat(64))
            .fetch_one(&pool)
            .await
            .unwrap(),
            "consumed"
        );
        assert!(sqlx::query("INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'plan-v35',?,'managed-v35-expired','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','archive',1,1,?,?,'planned',NULL,NULL,1,1)")
            .bind("1".repeat(64))
            .bind(&plan_digest)
            .bind("d".repeat(64))
            .bind("e".repeat(64))
            .execute(&pool)
            .await
            .is_err());
        let invalid_audit_values = [
            "zeroblob(64)".to_string(),
            format!("'{}'", "A".repeat(64)),
            format!("'{}'", "雪".repeat(64)),
            "''".to_string(),
            "NULL".to_string(),
        ];
        for column in ["audit_digest", "legacy_actor_digest", "sanitized_actor_id"] {
            for value in &invalid_audit_values {
                let values = match column {
                    "audit_digest" => format!("{value},?,?"),
                    "legacy_actor_digest" => format!("?,{value},?"),
                    "sanitized_actor_id" => format!("?,?,{value}"),
                    _ => unreachable!(),
                };
                let statement = format!(
                    "INSERT INTO mcp_profile_lifecycle_impact_v35_migration_audits(audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) VALUES ({values},'discarded_integrity',0)"
                );
                assert!(sqlx::query(&statement)
                    .bind("a".repeat(64))
                    .bind("b".repeat(64))
                    .execute(&pool)
                    .await
                    .is_err());
            }
        }
        for status in ["NULL", "zeroblob(1)", "1", "1.5", "'discarded_unknown'"] {
            let statement = format!(
                "INSERT INTO mcp_profile_lifecycle_impact_v35_migration_audits(audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) VALUES (?,?,?,{status},0)"
            );
            assert!(sqlx::query(&statement)
                .bind("a".repeat(64))
                .bind("b".repeat(64))
                .bind("c".repeat(64))
                .execute(&pool)
                .await
                .is_err());
        }
        for migrated_at_ms in ["NULL", "zeroblob(1)", "'0'", "0.5", "-1"] {
            let statement = format!(
                "INSERT INTO mcp_profile_lifecycle_impact_v35_migration_audits(audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) VALUES (?,?,?,'discarded_integrity',{migrated_at_ms})"
            );
            assert!(sqlx::query(&statement)
                .bind("a".repeat(64))
                .bind("b".repeat(64))
                .bind("c".repeat(64))
                .execute(&pool)
                .await
                .is_err());
        }
        pool.close().await;
    }

    #[tokio::test]
    async fn v35_lifecycle_impact_upgrade_audits_null_and_non_text_legacy_values() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("v35-null-legacy-lifecycle-impacts.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x72; 32],
            database_path_binding(&path).unwrap(),
        );
        create_repository_fixture_with_version(&path, signer.clone(), 34).await;
        let pool = existing_repository_pool(&path).await;
        let plan_digest = "a".repeat(64);
        let expires_at_ms = profile_lifecycle_test_expiry_ms();
        sqlx::query("INSERT INTO mcp_profiles(profile_id,name,description,revision,archived,created_at_ms,updated_at_ms,credential_references_json) VALUES ('profile-v35-null','Profile','fixture',1,0,1,1,'[]')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO managed_mcps(managed_mcp_id,mcp_id,installation_scope,state_json,revision,created_at_ms,updated_at_ms) VALUES ('managed-v35-null','mcp-v35-null','user','{}',1,1,1)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO mcp_profile_apply_plans(plan_id,profile_id,profile_revision,plan_digest,snapshot_json,actor,expires_at_ms,idempotency_key,request_digest,created_at_ms) VALUES ('plan-v35-null','profile-v35-null',1,?,'{}','actor-v35-null',?,'plan-key-v35-null',?,1)")
            .bind(&plan_digest)
            .bind(expires_at_ms)
            .bind("b".repeat(64))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,'plan-v35-null',?,'managed-v35-null','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','archive',?,1,?,?,'planned',NULL,NULL,1,1)")
            .bind("c".repeat(64))
            .bind(&plan_digest)
            .bind(expires_at_ms)
            .bind("d".repeat(64))
            .bind("e".repeat(64))
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &[
                "DROP TRIGGER mcp_profile_lifecycle_impacts_initial_state_is_planned",
                "DROP TRIGGER mcp_profile_lifecycle_impacts_parent_commitment_on_insert",
                "DROP TRIGGER mcp_profile_lifecycle_impacts_state_transition_is_monotonic",
                "DROP TRIGGER mcp_profile_lifecycle_impacts_parent_commitment_on_transition",
                "DROP TRIGGER mcp_profile_lifecycle_impacts_commitments_are_immutable",
                "DROP TRIGGER mcp_profile_lifecycle_impacts_updated_at_ms_is_monotonic",
                "DROP TRIGGER mcp_profile_lifecycle_impacts_confirmation_is_immutable",
                "DROP TRIGGER mcp_profile_lifecycle_impacts_consumption_is_immutable",
                "DROP TRIGGER mcp_profile_apply_plans_commitments_are_immutable_after_impact",
                "DROP INDEX mcp_profile_lifecycle_impacts_plan_id",
                "DROP INDEX mcp_profile_lifecycle_impacts_managed_mcp_id",
                "DROP INDEX mcp_profile_lifecycle_impacts_expires_at_ms",
                "ALTER TABLE mcp_profile_lifecycle_impacts RENAME TO mcp_profile_lifecycle_impacts_v34_fixture_source",
                r#"CREATE TABLE mcp_profile_lifecycle_impacts (
                    impact_digest,
                    plan_id,
                    plan_digest,
                    managed_mcp_id,
                    actor_id,
                    operation,
                    expires_at_ms,
                    managed_revision,
                    manifest_evidence_digest,
                    projection_evidence_digest,
                    state,
                    confirmed_at_ms,
                    consumed_at_ms,
                    created_at_ms,
                    updated_at_ms
                )"#,
                "INSERT INTO mcp_profile_lifecycle_impacts SELECT * FROM mcp_profile_lifecycle_impacts_v34_fixture_source",
                "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (NULL,NULL,NULL,NULL,NULL,'archive',NULL,NULL,NULL,NULL,'planned',NULL,NULL,NULL,NULL)",
                "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) SELECT zeroblob(64),'plan-v35-null',plan_digest,'managed-v35-null',zeroblob(64),'archive',expires_at_ms,1,'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee','planned',NULL,NULL,1,1 FROM mcp_profile_apply_plans WHERE plan_id='plan-v35-null'",
                "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) SELECT 'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff','plan-v35-null',plan_digest,'managed-v35-null','actor=legacy-token','archive',expires_at_ms,1,'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee','planned',NULL,NULL,1,1 FROM mcp_profile_apply_plans WHERE plan_id='plan-v35-null'",
                "DROP TABLE mcp_profile_lifecycle_impacts_v34_fixture_source",
                "CREATE INDEX mcp_profile_lifecycle_impacts_plan_id ON mcp_profile_lifecycle_impacts(plan_id)",
                "CREATE INDEX mcp_profile_lifecycle_impacts_managed_mcp_id ON mcp_profile_lifecycle_impacts(managed_mcp_id)",
                "CREATE INDEX mcp_profile_lifecycle_impacts_expires_at_ms ON mcp_profile_lifecycle_impacts(expires_at_ms)",
                "CREATE TRIGGER mcp_profile_lifecycle_impacts_initial_state_is_planned BEFORE INSERT ON mcp_profile_lifecycle_impacts BEGIN SELECT 1; END",
                "CREATE TRIGGER mcp_profile_lifecycle_impacts_parent_commitment_on_insert BEFORE INSERT ON mcp_profile_lifecycle_impacts BEGIN SELECT 1; END",
                "CREATE TRIGGER mcp_profile_lifecycle_impacts_state_transition_is_monotonic BEFORE INSERT ON mcp_profile_lifecycle_impacts BEGIN SELECT 1; END",
                "CREATE TRIGGER mcp_profile_lifecycle_impacts_parent_commitment_on_transition BEFORE INSERT ON mcp_profile_lifecycle_impacts BEGIN SELECT 1; END",
                "CREATE TRIGGER mcp_profile_lifecycle_impacts_commitments_are_immutable BEFORE INSERT ON mcp_profile_lifecycle_impacts BEGIN SELECT 1; END",
                "CREATE TRIGGER mcp_profile_lifecycle_impacts_updated_at_ms_is_monotonic BEFORE INSERT ON mcp_profile_lifecycle_impacts BEGIN SELECT 1; END",
                "CREATE TRIGGER mcp_profile_lifecycle_impacts_confirmation_is_immutable BEFORE INSERT ON mcp_profile_lifecycle_impacts BEGIN SELECT 1; END",
                "CREATE TRIGGER mcp_profile_lifecycle_impacts_consumption_is_immutable BEFORE INSERT ON mcp_profile_lifecycle_impacts BEGIN SELECT 1; END",
                "CREATE TRIGGER mcp_profile_apply_plans_commitments_are_immutable_after_impact BEFORE INSERT ON mcp_profile_apply_plans BEGIN SELECT 1; END",
            ],
        )
        .await;

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;

        let pool = existing_repository_pool(&path).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mcp_profile_lifecycle_impacts")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM mcp_profile_lifecycle_impact_v35_migration_audits"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            3
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mcp_profile_lifecycle_impact_v35_migration_audits WHERE status='discarded_integrity' AND typeof(audit_digest)='text' AND typeof(legacy_actor_digest)='text' AND typeof(sanitized_actor_id)='text' AND typeof(migrated_at_ms)='integer'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            3
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mcp_profile_lifecycle_impact_v35_migration_audits WHERE ? IN (audit_digest,legacy_actor_digest,sanitized_actor_id)")
                .bind("actor=legacy-token")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        let audit_table_sql = sqlx::query_scalar::<_, String>("SELECT sql FROM sqlite_master WHERE type='table' AND name='mcp_profile_lifecycle_impact_v35_migration_audits'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(audit_table_sql.contains("typeof(status)='text'"));
        assert!(audit_table_sql.contains("typeof(migrated_at_ms)='integer'"));
        pool.close().await;

        let reopened = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap();
        reopened.close().await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(
            sqlite_table_row_count(&path, "mcp_profile_lifecycle_impact_v35_migration_audits")
                .await,
            3
        );
    }

    #[tokio::test]
    async fn v36_rebuilds_audits_with_strict_storage_and_unique_legacy_dispositions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v35-to-v36-audit-rebuild.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x73; 32],
            database_path_binding(&path).unwrap(),
        );
        create_repository_fixture_with_version(&path, signer.clone(), 35).await;
        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &[
                "INSERT INTO mcp_profile_lifecycle_impact_v35_migration_audits(audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) VALUES ('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc','discarded_integrity',0)",
                "INSERT INTO mcp_profile_lifecycle_impact_v34_migration_audits(audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) VALUES ('dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee','ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff','discarded_integrity',zeroblob(1))",
                "PRAGMA ignore_check_constraints=ON",
                "INSERT INTO mcp_profile_lifecycle_impact_v34_migration_audits(audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) VALUES ('1111111111111111111111111111111111111111111111111111111111111111','2222222222222222222222222222222222222222222222222222222222222222','3333333333333333333333333333333333333333333333333333333333333333','discarded_unknown',0)",
                "INSERT INTO mcp_profile_lifecycle_impact_v34_migration_audits(audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) VALUES ('4444444444444444444444444444444444444444444444444444444444444444','5555555555555555555555555555555555555555555555555555555555555555','6666666666666666666666666666666666666666666666666666666666666666','discarded_unknown',0)",
                "PRAGMA ignore_check_constraints=OFF",
            ],
        )
        .await;

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        let pool = existing_repository_pool(&path).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mcp_profile_lifecycle_impact_v35_migration_audits WHERE audit_digest='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mcp_profile_lifecycle_impact_v36_migration_dispositions WHERE migration_domain='profile_lifecycle_v34_audit'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            3
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(DISTINCT disposition_digest) FROM mcp_profile_lifecycle_impact_v36_migration_dispositions WHERE migration_domain='profile_lifecycle_v34_audit'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            3
        );
        for value in ["NULL", "'0'", "'-1'", "zeroblob(1)", "0.5", "-1"] {
            let statement = format!(
                "INSERT INTO mcp_profile_lifecycle_impact_v35_migration_audits(audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) VALUES ('7777777777777777777777777777777777777777777777777777777777777777','8888888888888888888888888888888888888888888888888888888888888888','9999999999999999999999999999999999999999999999999999999999999999','discarded_integrity',{value})"
            );
            assert!(sqlx::query(&statement).execute(&pool).await.is_err());
        }
        for status in ["NULL", "zeroblob(1)", "'discarded_unknown'"] {
            let statement = format!(
                "INSERT INTO mcp_profile_lifecycle_impact_v35_migration_audits(audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) VALUES ('7777777777777777777777777777777777777777777777777777777777777777','8888888888888888888888888888888888888888888888888888888888888888','9999999999999999999999999999999999999999999999999999999999999999',{status},0)"
            );
            assert!(sqlx::query(&statement).execute(&pool).await.is_err());
        }
        pool.close().await;

        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        reopened.close().await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(
            sqlite_table_row_count(
                &path,
                "mcp_profile_lifecycle_impact_v36_migration_dispositions"
            )
            .await,
            3
        );
        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &[
                "DROP TABLE mcp_profile_lifecycle_impact_v36_migration_dispositions",
                "CREATE TABLE mcp_profile_lifecycle_impact_v36_migration_dispositions (disposition_digest TEXT PRIMARY KEY)",
            ],
        )
        .await;
        assert!(
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn v38_rejects_non_positive_v37_source_rowids_without_publishing_checkpoint() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v37-to-v38-rowid-rejection.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x74; 32],
            database_path_binding(&path).unwrap(),
        );
        create_repository_fixture_with_version(&path, signer.clone(), 37).await;
        let source_table = "mcp_profile_lifecycle_impact_v34_migration_audits_v35_legacy";
        let statements = [-1_i64, 0, 9]
            .into_iter()
            .map(|source_rowid| {
                let digest = migrations::profile_lifecycle_v37_disposition_digest(
                    "profile_lifecycle_v34_audit",
                    source_table,
                    source_rowid,
                    "invalid_audit_status_text_time_integer",
                );
                format!(
                    "INSERT INTO mcp_profile_lifecycle_impact_v36_migration_dispositions(disposition_digest,migration_domain,source_table,source_rowid,legacy_row_type,status,migrated_at_ms) VALUES ('{digest}','profile_lifecycle_v34_audit','{source_table}',{source_rowid},'invalid_audit_status_text_time_integer','discarded_legacy_audit',0)"
                )
            })
            .collect::<Vec<_>>();
        let statement_refs = statements.iter().map(String::as_str).collect::<Vec<_>>();
        execute_sql_batch_and_publish_integrity(&path, signer.clone(), &statement_refs).await;
        let checkpoint_before = signer.checkpoint().unwrap();
        let commit_count_before = sqlite_integrity_commit_count(&path).await;

        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert!(
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .is_err()
        );
        assert_eq!(
            sqlite_integrity_commit_count(&path).await,
            commit_count_before
        );
        assert_eq!(signer.checkpoint().unwrap(), checkpoint_before);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert!(
            !sqlite_schema_object_exists(
                &path,
                "table",
                "mcp_profile_lifecycle_impact_v38_disposition_canonicals"
            )
            .await
        );
        let pool = existing_repository_pool(&path).await;
        let rowids = sqlx::query_scalar::<_, i64>(
            "SELECT source_rowid FROM mcp_profile_lifecycle_impact_v36_migration_dispositions WHERE migration_domain='profile_lifecycle_v34_audit' ORDER BY source_rowid",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rowids, vec![-1, 0, 9]);
        pool.close().await;
        assert!(
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn v36_to_v38_without_legacy_audits_is_reopenable_and_recovery_eligible() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v36-to-v37-no-legacy-attestation.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x75; 32],
            database_path_binding(&path).unwrap(),
        );
        create_repository_fixture_with_version(&path, signer.clone(), 36).await;

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        assert_eq!(
            repository.recovery_eligibility().await.unwrap(),
            RecoveryEligibility::Eligible
        );
        repository.close().await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(
            sqlite_table_row_count(&path, "mcp_profile_lifecycle_audit_integrity_attestations",)
                .await,
            0
        );
        let reopened = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap();
        reopened.close().await;
    }

    #[tokio::test]
    async fn v38_seals_live_and_canonical_records_against_direct_sql_mutations() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v38-direct-sql-seal.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x76; 32],
            database_path_binding(&path).unwrap(),
        );
        create_repository_fixture_with_version(&path, signer.clone(), 35).await;
        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &[
                "PRAGMA ignore_check_constraints=ON",
                "INSERT INTO mcp_profile_lifecycle_impact_v34_migration_audits(audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) VALUES ('1111111111111111111111111111111111111111111111111111111111111111','2222222222222222222222222222222222222222222222222222222222222222','3333333333333333333333333333333333333333333333333333333333333333','discarded_unknown',0)",
                "PRAGMA ignore_check_constraints=OFF",
                "INSERT INTO mcp_profile_lifecycle_impact_v35_migration_audits(audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) VALUES ('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc','discarded_integrity',0)",
            ],
        )
        .await;
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        assert_eq!(
            repository.recovery_eligibility().await.unwrap(),
            RecoveryEligibility::Blocked
        );
        repository.close().await;

        let pool = existing_repository_pool(&path).await;
        let disposition_digest = sqlx::query_scalar::<_, String>(
            "SELECT disposition_digest FROM mcp_profile_lifecycle_impact_v36_migration_dispositions",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let disposition_values = format!(
            "('{disposition_digest}','profile_lifecycle_v34_audit','mcp_profile_lifecycle_impact_v34_migration_audits_v35_legacy',1,'invalid_audit_status_text_time_integer','discarded_legacy_audit',0)"
        );
        let attestation_values = "('profile_lifecycle_v35_invalid_audit','profile_lifecycle_v35_invalid_audit',35,35,'unverifiable_legacy_aggregation',1,0)";
        for recursive_triggers in ["OFF", "ON"] {
            sqlx::query(&format!("PRAGMA recursive_triggers={recursive_triggers}"))
                .execute(&pool)
                .await
                .unwrap();
            for (table, key, values, update) in [
                (
                    "mcp_profile_lifecycle_impact_v36_migration_dispositions",
                    "disposition_digest",
                    disposition_values.as_str(),
                    "status='discarded_legacy_audit'",
                ),
                (
                    "mcp_profile_lifecycle_impact_v38_disposition_canonicals",
                    "disposition_digest",
                    disposition_values.as_str(),
                    "status='discarded_legacy_audit'",
                ),
                (
                    "mcp_profile_lifecycle_audit_integrity_attestations",
                    "attestation_id",
                    attestation_values,
                    "observed_record_count=1",
                ),
                (
                    "mcp_profile_lifecycle_audit_integrity_attestation_canonicals",
                    "attestation_id",
                    attestation_values,
                    "observed_record_count=1",
                ),
            ] {
                let columns = if key == "disposition_digest" {
                    "disposition_digest,migration_domain,source_table,source_rowid,legacy_row_type,status,migrated_at_ms"
                } else {
                    "attestation_id,migration_domain,version_from,version_through,status,observed_record_count,created_at_ms"
                };
                for statement in [
                    format!("INSERT INTO {table}({columns}) VALUES {values}"),
                    format!("UPDATE {table} SET {update}"),
                    format!("DELETE FROM {table}"),
                    format!("INSERT OR REPLACE INTO {table}({columns}) VALUES {values}"),
                    format!(
                        "INSERT INTO {table}({columns}) VALUES {values} ON CONFLICT({key}) DO UPDATE SET {update}"
                    ),
                ] {
                    assert!(sqlx::query(&statement).execute(&pool).await.is_err());
                }
            }
            for source_rowid in ["NULL", "'1'", "zeroblob(1)", "0.5"] {
                for table in [
                    "mcp_profile_lifecycle_impact_v36_migration_dispositions",
                    "mcp_profile_lifecycle_impact_v38_disposition_canonicals",
                ] {
                    let statement = format!(
                        "INSERT INTO {table}(disposition_digest,migration_domain,source_table,source_rowid,legacy_row_type,status,migrated_at_ms) VALUES ('eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee','profile_lifecycle_v34_audit','mcp_profile_lifecycle_impact_v34_migration_audits_v35_legacy',{source_rowid},'invalid_audit_status_text_time_integer','discarded_legacy_audit',0)"
                    );
                    assert!(sqlx::query(&statement).execute(&pool).await.is_err());
                }
            }
            for digest in ["'not-a-digest'", "zeroblob(64)"] {
                for table in [
                    "mcp_profile_lifecycle_impact_v36_migration_dispositions",
                    "mcp_profile_lifecycle_impact_v38_disposition_canonicals",
                ] {
                    let statement = format!(
                        "INSERT INTO {table}(disposition_digest,migration_domain,source_table,source_rowid,legacy_row_type,status,migrated_at_ms) VALUES ({digest},'profile_lifecycle_v34_audit','mcp_profile_lifecycle_impact_v34_migration_audits_v35_legacy',1,'invalid_audit_status_text_time_integer','discarded_legacy_audit',0)"
                    );
                    assert!(sqlx::query(&statement).execute(&pool).await.is_err());
                }
            }
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM mcp_profile_lifecycle_impact_v36_migration_dispositions d JOIN mcp_profile_lifecycle_impact_v38_disposition_canonicals c USING(disposition_digest) WHERE d.source_rowid=c.source_rowid AND d.status=c.status",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM mcp_profile_lifecycle_audit_integrity_attestations a JOIN mcp_profile_lifecycle_audit_integrity_attestation_canonicals c USING(attestation_id) WHERE a.observed_record_count=c.observed_record_count",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        pool.close().await;
    }

    #[tokio::test]
    async fn v38_recovery_eligibility_blocks_missing_or_forged_canonical_dispositions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v36-to-v38-recovery-eligibility.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x77; 32],
            database_path_binding(&path).unwrap(),
        );
        create_repository_fixture_with_version(&path, signer.clone(), 36).await;
        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &["INSERT INTO mcp_profile_lifecycle_impact_v36_migration_dispositions(disposition_digest,migration_domain,source_rowid,legacy_row_type,status,migrated_at_ms) VALUES ('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','profile_lifecycle_v34_audit',1,'invalid_audit_status_text_time_integer','discarded_legacy_audit',0)"],
        )
        .await;
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        assert_eq!(
            repository.recovery_eligibility().await.unwrap(),
            RecoveryEligibility::Eligible
        );

        let pool = existing_repository_pool(&path).await;
        sqlx::query(
            "DROP TRIGGER mcp_profile_lifecycle_v38_disposition_canonical_cannot_be_updated",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE mcp_profile_lifecycle_impact_v38_disposition_canonicals SET source_rowid=2",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        assert_eq!(
            repository.recovery_eligibility().await.unwrap(),
            RecoveryEligibility::Blocked
        );
        let pool = existing_repository_pool(&path).await;
        sqlx::query(
            "DROP TRIGGER mcp_profile_lifecycle_v38_disposition_canonical_cannot_be_deleted",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM mcp_profile_lifecycle_impact_v38_disposition_canonicals")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        assert_eq!(
            repository.recovery_eligibility().await.unwrap(),
            RecoveryEligibility::Blocked
        );
        repository.close().await;
    }

    #[tokio::test]
    async fn legacy_v26_manifest_blobs_upgrade_to_v27_defaults_source_metadata_to_local() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("legacy-v26-manifest-source-upgrade.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x32; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let verified = parse_manifest(REMOTE.as_bytes()).unwrap();
        let digest = verified.digest().to_string();
        repository
            .save_manifest(&ManifestRecord {
                verified,
                proof: ManifestProof::LocalBytes,
                trust_tier: TrustTier::Local,
                source_metadata: Default::default(),
                created_at_ms: 1,
            })
            .await
            .unwrap();
        repository.close().await;

        rewrite_manifest_blobs_as_legacy_v26(&path, signer.clone()).await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert!(!sqlite_table_has_column(&path, "manifest_blobs", "source_ref_json").await);

        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let manifest = reopened.get_manifest(&digest).await.unwrap();
        reopened.close().await;

        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert!(sqlite_table_has_column(&path, "manifest_blobs", "source_ref_json").await);
        assert_eq!(
            manifest.source_metadata,
            ManifestSourceMetadata::local_persistence()
        );
        assert_eq!(manifest.trust_tier, TrustTier::Local);
    }

    #[tokio::test]
    async fn manifest_source_metadata_round_trips_through_repository() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("manifest-source-metadata-roundtrip.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x7a; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        let verified = parse_manifest(REMOTE.as_bytes()).unwrap();
        let digest = verified.digest().to_string();
        let source_metadata = ManifestSourceMetadata {
            source_ref: SourceRef::VerifiedSourceCatalog {
                source_id: "catalog-source".to_string(),
            },
            import_kind: SourceImportKind::VerifiedSourceCatalog,
            release_id: Some("release-2026-07".to_string()),
            origin_provenance: OriginProvenance {
                verified_source_document: Some(VerifiedSourceDocumentRef {
                    source_id: "catalog-source".to_string(),
                    document_digest: "a".repeat(64),
                    canonical_digest: "b".repeat(64),
                    signed_digest: "c".repeat(64),
                    binding_digest: "d".repeat(64),
                    signature_kids: vec!["kid-1".to_string()],
                }),
            },
            update_channel: UpdateChannel::Named {
                name: "stable".to_string(),
            },
        };
        let proof = ManifestProof::Catalog {
            index_digest: "e".repeat(64),
            declared_manifest_digest: digest.clone(),
            signature: Some(SignatureEvidence {
                algorithm: "ed25519".to_string(),
                signing_identity: "catalog-root".to_string(),
                signature: "sig-proof".to_string(),
            }),
        };
        repository
            .save_manifest(&ManifestRecord {
                verified,
                proof: proof.clone(),
                trust_tier: TrustTier::Official,
                source_metadata: source_metadata.clone(),
                created_at_ms: 7,
            })
            .await
            .unwrap();

        let fetched = repository.get_manifest(&digest).await.unwrap();
        repository.close().await;

        assert_eq!(fetched.proof, proof);
        assert_eq!(fetched.source_metadata, source_metadata);
        assert_eq!(fetched.trust_tier, TrustTier::Official);
    }

    #[tokio::test]
    async fn legacy_v17_managed_enrollment_schema_upgrades_to_v18_without_backfilling_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-v17-enrollment-upgrade.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x33; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let verified = parse_manifest(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "id": "legacy-v17-managed-enrollment",
                "version": "1.0.0",
                "name": "Legacy V17 Enrollment",
                "description": "fixture",
                "publisher": {"id": "local", "name": "Local"},
                "license": {"spdx": "MIT"},
                "capabilities": ["tools"],
                "permissions": [],
                "distribution": {"type": "remote_http"},
                "transport": {"type": "streamable_http", "url": "https://example.com/mcp"},
                "auth": {
                    "type": "environment",
                    "environment_key": "AUTH_TOKEN",
                    "credential_name": "legacy-token"
                },
                "health_check": {"type": "mcp_initialize", "timeout_seconds": 30},
                "owned_files": [],
                "uninstall": {"mode": "remove_owned_files_only", "preserve_user_data": true}
            }))
            .unwrap(),
        )
        .unwrap();
        let digest = verified.digest().to_string();
        let adapter_evidence = crate::mcp_platform::task::AdapterEvidence {
            adapter_id: "test-adapter".to_string(),
            adapter_version: "1.0.0".to_string(),
            compatible_for_recovery: true,
            resume_safe: true,
        };
        let reference_digest = "1".repeat(64);
        let authority_evidence_digest = "a".repeat(64);
        let authority =
            crate::mcp_platform::repository::ManagedCredentialEnrollmentAuthoritySummary {
                provider_id: "in-memory-test".to_string(),
                writer_mode: "best_effort_single_process".to_string(),
            };
        repository
            .save_manifest(&ManifestRecord {
                verified,
                proof: ManifestProof::LocalBytes,
                trust_tier: TrustTier::Local,
                source_metadata: Default::default(),
                created_at_ms: 1,
            })
            .await
            .unwrap();
        repository
            .register_managed_mcp(crate::mcp_platform::repository::RegisterManagedMcp {
                managed_mcp_id: "legacy_v17_managed",
                mcp_id: "legacy-v17-managed-enrollment",
                installation_scope: "user",
                distribution_adapter: "remote_http",
                manifest_digest: &digest,
                version: "1.0.0",
                task_id: "task_fixture",
                adapter_evidence: &adapter_evidence,
                now_ms: 1,
            })
            .await
            .unwrap();
        repository
            .save_managed_credential_enrollment(
                crate::mcp_platform::repository::SaveManagedCredentialEnrollment {
                    managed_mcp_id: "legacy_v17_managed",
                    manifest_digest: &digest,
                    auth_schema_id: "static_env_secret",
                    credential_reference: "enr.legacy-v17",
                    reference_digest: &reference_digest,
                    authority: &authority,
                    authority_evidence_digest: &authority_evidence_digest,
                    now_ms: 2,
                },
            )
            .await
            .unwrap();
        repository.close().await;

        rewrite_managed_enrollments_as_legacy_v17(&path).await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert!(
            !sqlite_table_has_column(
                &path,
                "managed_credential_enrollments",
                "authority_evidence_digest"
            )
            .await
        );

        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let enrollment = reopened
            .get_managed_credential_enrollment("legacy_v17_managed")
            .await
            .unwrap()
            .unwrap();
        reopened.close().await;

        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert!(
            sqlite_table_has_column(
                &path,
                "managed_credential_enrollments",
                "authority_evidence_digest"
            )
            .await
        );
        assert!(enrollment.authority_evidence_digest.is_none());
    }

    #[tokio::test]
    async fn legacy_v18_profile_confirmation_schema_upgrades_to_v19_and_is_writable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("legacy-v18-profile-confirmation-upgrade.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x34; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;

        execute_sql_batch(
            &path,
            &[
                "DROP TABLE mcp_profile_apply_confirmations",
                "DELETE FROM schema_version WHERE version = 19",
            ],
        )
        .await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert!(
            !sqlite_table_has_column(
                &path,
                "mcp_profile_apply_confirmations",
                "confirmation_hash"
            )
            .await
        );

        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO mcp_profiles(profile_id,name,description,revision,archived,created_at_ms,updated_at_ms,credential_references_json) VALUES ('profile_v19','Profile','fixture',1,0,1,1,'[]')",
        )
        .execute(&reopened.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mcp_profile_apply_plans(plan_id,profile_id,profile_revision,plan_digest,snapshot_json,actor,expires_at_ms,idempotency_key,request_digest,created_at_ms) VALUES ('plan_v19','profile_v19',1,?,'{}','local_authenticated_client',10,'plan-key-v19',?,1)",
        )
        .bind("a".repeat(64))
        .bind("b".repeat(64))
        .execute(&reopened.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mcp_profile_apply_confirmations(confirmation_hash,plan_id,actor,expires_at_ms,consumed_at_ms,created_at_ms) VALUES (?, 'plan_v19', 'local_authenticated_client', 10, NULL, 1)",
        )
        .bind("c".repeat(64))
        .execute(&reopened.pool)
        .await
        .unwrap();
        let confirmation_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM mcp_profile_apply_confirmations WHERE plan_id='plan_v19'",
        )
        .fetch_one(&reopened.pool)
        .await
        .unwrap();
        reopened.close().await;

        assert_eq!(confirmation_count, 1);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert!(
            sqlite_table_has_column(
                &path,
                "mcp_profile_apply_confirmations",
                "confirmation_hash"
            )
            .await
        );
    }

    #[tokio::test]
    async fn legacy_v14_projection_mutation_schema_upgrades_atomically_to_v15() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-v14-upgrade.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x38; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        rewrite_projection_mutations_as_legacy_v14(&path).await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );

        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        reopened.close().await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert!(
            sqlite_table_has_column(&path, "projection_mutations", "writer_witness_version").await
        );
        assert!(sqlite_table_has_column(&path, "projection_mutations", "writer_provider_id").await);
        assert!(
            sqlite_table_has_column(&path, "projection_mutations", "writer_binding_domain").await
        );
        assert!(
            sqlite_table_has_column(
                &path,
                "projection_mutations",
                "writer_observed_state_digest"
            )
            .await
        );
    }

    #[tokio::test]
    async fn brand_new_repository_open_migrates_explicitly_to_v26() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("brand-new-v26.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x62; 32],
            database_path_binding(&path).unwrap(),
        );

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        repository.close().await;

        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert!(
            sqlite_schema_object_exists(
                &path,
                "table",
                "mcp_intake_remote_inspection_reservations"
            )
            .await
        );
    }

    #[tokio::test]
    async fn pristine_v25_repository_open_advances_once_to_v26() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pristine-v25-to-v26.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x63; 32],
            database_path_binding(&path).unwrap(),
        );

        create_repository_fixture_with_version(&path, signer.clone(), 25).await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1_i64..=25).collect::<Vec<_>>()
        );
        assert!(
            !sqlite_schema_object_exists(
                &path,
                "table",
                "mcp_intake_remote_inspection_reservations"
            )
            .await
        );

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        repository.close().await;

        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert!(
            sqlite_schema_object_exists(
                &path,
                "table",
                "mcp_intake_remote_inspection_reservations"
            )
            .await
        );
    }

    #[tokio::test]
    async fn v26_reopen_does_not_append_schema_version_rows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v26-reopen.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x64; 32],
            database_path_binding(&path).unwrap(),
        );

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;

        let schema_versions_before = sqlite_schema_versions(&path).await;
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let reopened = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap();
        reopened.close().await;

        assert_eq!(sqlite_schema_versions(&path).await, schema_versions_before);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
    }

    #[tokio::test]
    async fn v25_complete_v26_schema_without_version_row_fails_closed_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v25-complete-v26-no-row.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x65; 32],
            database_path_binding(&path).unwrap(),
        );

        create_repository_fixture_with_version(&path, signer.clone(), 25).await;
        apply_v26_schema_without_version_and_publish_integrity(&path, signer.clone(), &[]).await;
        let schema_versions_before = sqlite_schema_versions(&path).await;
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(sqlite_schema_versions(&path).await, schema_versions_before);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert!(
            sqlite_schema_object_exists(
                &path,
                "table",
                "mcp_intake_remote_inspection_reservations"
            )
            .await
        );
    }

    #[tokio::test]
    async fn v25_partial_v26_schema_without_version_row_fails_closed_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v25-partial-v26-no-row.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x66; 32],
            database_path_binding(&path).unwrap(),
        );

        create_repository_fixture_with_version(&path, signer.clone(), 25).await;
        apply_v26_schema_without_version_and_publish_integrity(
            &path,
            signer.clone(),
            &["DROP TRIGGER mcp_intake_remote_inspection_reserved_event_v26"],
        )
        .await;
        let schema_versions_before = sqlite_schema_versions(&path).await;
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(sqlite_schema_versions(&path).await, schema_versions_before);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert!(
            !sqlite_schema_object_exists(
                &path,
                "trigger",
                "mcp_intake_remote_inspection_reserved_event_v26"
            )
            .await
        );
        assert!(
            sqlite_schema_object_exists(
                &path,
                "table",
                "mcp_intake_remote_inspection_reservations"
            )
            .await
        );
    }

    #[tokio::test]
    async fn v25_extra_v26_table_without_version_row_fails_closed_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v25-extra-v26-table-no-row.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x67; 32],
            database_path_binding(&path).unwrap(),
        );

        create_repository_fixture_with_version(&path, signer.clone(), 25).await;
        apply_v26_schema_without_version_and_publish_integrity(
            &path,
            signer.clone(),
            &["CREATE TABLE mcp_intake_remote_inspection_shadow_table_v26(dummy TEXT NOT NULL)"],
        )
        .await;
        let schema_versions_before = sqlite_schema_versions(&path).await;
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(sqlite_schema_versions(&path).await, schema_versions_before);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert!(
            sqlite_schema_object_exists(
                &path,
                "table",
                "mcp_intake_remote_inspection_shadow_table_v26"
            )
            .await
        );
    }

    #[tokio::test]
    async fn v25_extra_v26_trigger_without_version_row_fails_closed_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v25-extra-v26-trigger-no-row.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x68; 32],
            database_path_binding(&path).unwrap(),
        );

        create_repository_fixture_with_version(&path, signer.clone(), 25).await;
        apply_v26_schema_without_version_and_publish_integrity(
            &path,
            signer.clone(),
            &[
                r#"CREATE TRIGGER mcp_intake_remote_inspection_shadow_trigger_v26
                BEFORE INSERT ON mcp_intake_remote_inspection_reservations
                BEGIN
                    SELECT RAISE(ABORT, 'shadow_v26');
                END"#,
            ],
        )
        .await;
        let schema_versions_before = sqlite_schema_versions(&path).await;
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(sqlite_schema_versions(&path).await, schema_versions_before);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert!(
            sqlite_schema_object_exists(
                &path,
                "trigger",
                "mcp_intake_remote_inspection_shadow_trigger_v26"
            )
            .await
        );
    }

    #[tokio::test]
    async fn v26_reopen_with_extra_v26_index_fails_closed_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v26-extra-index.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x69; 32],
            database_path_binding(&path).unwrap(),
        );

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &["CREATE INDEX mcp_intake_remote_inspection_shadow_index_v26 ON mcp_intake_remote_inspection_reservations(reservation_id)"],
        )
        .await;
        let schema_versions_before = sqlite_schema_versions(&path).await;
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(sqlite_schema_versions(&path).await, schema_versions_before);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert!(
            sqlite_schema_object_exists(
                &path,
                "index",
                "mcp_intake_remote_inspection_shadow_index_v26"
            )
            .await
        );
    }

    #[tokio::test]
    async fn projection_schema_validator_accepts_pristine_v25_without_side_effects() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("validator-pristine-v25.db");

        create_schema_fixture_with_version(&path, 25).await;
        let schema_versions_before = sqlite_schema_versions(&path).await;
        let pool = existing_repository_pool(&path).await;

        validate_current_v15_projection_schema(&pool).await.unwrap();

        pool.close().await;
        assert_eq!(sqlite_schema_versions(&path).await, schema_versions_before);
        assert!(
            !sqlite_schema_object_exists(
                &path,
                "table",
                "mcp_intake_remote_inspection_reservations"
            )
            .await
        );
    }

    #[tokio::test]
    async fn projection_schema_validator_rejects_v25_with_v26_objects_without_side_effects() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("validator-v25-with-v26.db");

        create_schema_fixture_with_version(&path, 25).await;
        let pool = existing_repository_pool(&path).await;
        let mut transaction = pool.begin().await.unwrap();
        migrations::apply_v26(&mut transaction).await.unwrap();
        transaction.commit().await.unwrap();
        pool.close().await;

        let schema_versions_before = sqlite_schema_versions(&path).await;
        let validation_pool = existing_repository_pool(&path).await;
        let error = validate_current_v15_projection_schema(&validation_pool)
            .await
            .unwrap_err();
        validation_pool.close().await;

        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(sqlite_schema_versions(&path).await, schema_versions_before);
        assert!(
            sqlite_schema_object_exists(
                &path,
                "table",
                "mcp_intake_remote_inspection_reservations"
            )
            .await
        );
    }

    #[tokio::test]
    async fn downshifted_v15_schema_version_row_fails_closed_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("downshift-v15.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x3d; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        let commits_before = sqlite_integrity_commit_count(&path).await;
        mutate_schema_versions(&path, "DELETE FROM schema_version WHERE version = ?", 15).await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
    }

    #[tokio::test]
    async fn security_state_digest_changes_when_schema_version_rows_change() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("schema-digest.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x61; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        repository.close().await;

        let pool = SqliteMcpPlatformRepository::readonly_existing_repository_pool(&path)
            .await
            .unwrap();
        let mut before_tx = pool.begin().await.unwrap();
        let digest_before = security_state_digest(&mut before_tx).await.unwrap();
        before_tx.rollback().await.unwrap();
        pool.close().await;

        mutate_schema_versions(&path, "DELETE FROM schema_version WHERE version = ?", 15).await;

        let pool = SqliteMcpPlatformRepository::readonly_existing_repository_pool(&path)
            .await
            .unwrap();
        let mut after_tx = pool.begin().await.unwrap();
        let digest_after = security_state_digest(&mut after_tx).await.unwrap();
        after_tx.rollback().await.unwrap();
        pool.close().await;

        assert_ne!(digest_before, digest_after);
    }

    #[tokio::test]
    async fn partial_projection_witness_v2_schema_fails_closed_without_advancing_schema_version() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("partial-v14.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x39; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        rewrite_projection_mutations_with_partial_witness_v2_column(&path).await;
        let columns_before = sqlite_table_columns(&path, "projection_mutations").await;
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(
            sqlite_table_columns(&path, "projection_mutations").await,
            columns_before
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
    }

    #[tokio::test]
    async fn projection_witness_v2_schema_missing_active_index_fails_closed_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing-active-index.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x3e; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        rewrite_projection_mutations_with_current_witness_shape(
            &path,
            r#"CREATE TABLE projection_mutations (
                mutation_id INTEGER PRIMARY KEY AUTOINCREMENT,
                managed_mcp_id TEXT NOT NULL,
                expected_revision INTEGER NOT NULL,
                previous_enabled INTEGER NOT NULL CHECK(previous_enabled IN (0,1)),
                desired_enabled INTEGER NOT NULL CHECK(desired_enabled IN (0,1)),
                status TEXT NOT NULL CHECK(status IN ('started','config_committed','committed','recovery_required','resolved')),
                writer_runtime_id TEXT,
                writer_sink_id TEXT,
                writer_anchor_instance_id TEXT,
                writer_anchor_path_binding TEXT CHECK(writer_anchor_path_binding IS NULL OR length(writer_anchor_path_binding) = 64),
                writer_anchor_key_epoch INTEGER CHECK(writer_anchor_key_epoch IS NULL OR writer_anchor_key_epoch > 0),
                writer_anchor_sequence INTEGER CHECK(writer_anchor_sequence IS NULL OR writer_anchor_sequence >= 0),
                writer_anchor_root TEXT CHECK(writer_anchor_root IS NULL OR length(writer_anchor_root) = 64),
                writer_witness_version INTEGER CHECK(writer_witness_version IS NULL OR writer_witness_version = 2),
                writer_provider_id TEXT CHECK(writer_provider_id IS NULL OR writer_provider_id IN ('system-keyring','in-memory-test')),
                writer_binding_domain TEXT,
                writer_observed_state_digest TEXT CHECK(writer_observed_state_digest IS NULL OR length(writer_observed_state_digest) = 64),
                writer_commitment TEXT CHECK(writer_commitment IS NULL OR length(writer_commitment) = 64),
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            )"#,
            false,
        )
        .await;
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
    }

    #[tokio::test]
    async fn projection_witness_v2_schema_with_extra_nonunique_index_fails_closed_without_repair() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("extra-nonunique-index.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x52; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        let commits_before = sqlite_integrity_commit_count(&path).await;
        let managed_before = sqlite_table_row_count(&path, "managed_mcps").await;
        let mutations_before = sqlite_table_row_count(&path, "projection_mutations").await;
        let mut expected_indexes = sqlite_table_indexes(&path, "projection_mutations").await;
        expected_indexes.push("projection_mutations_attack_nonunique".to_string());
        expected_indexes.sort();
        mutate_projection_mutations_indexes(
            &path,
            &["CREATE INDEX projection_mutations_attack_nonunique ON projection_mutations(status)"],
        )
        .await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert_eq!(
            sqlite_table_row_count(&path, "managed_mcps").await,
            managed_before
        );
        assert_eq!(
            sqlite_table_row_count(&path, "projection_mutations").await,
            mutations_before
        );
        assert_eq!(
            sqlite_table_indexes(&path, "projection_mutations").await,
            expected_indexes
        );
    }

    #[tokio::test]
    async fn projection_witness_v2_schema_with_extra_unique_index_blocking_reentry_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("extra-unique-index.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x53; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        mutate_projection_mutations_indexes(
            &path,
            &[
                "INSERT INTO projection_mutations(managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,created_at_ms,updated_at_ms) VALUES ('managed-blocked',0,1,0,'resolved',10,10)",
                "CREATE UNIQUE INDEX projection_mutations_attack_unique ON projection_mutations(managed_mcp_id)",
            ],
        )
        .await;
        let commits_before = sqlite_integrity_commit_count(&path).await;
        let mutations_before = sqlite_table_row_count(&path, "projection_mutations").await;
        let mut expected_indexes = sqlite_table_indexes(&path, "projection_mutations").await;
        expected_indexes.sort();

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert_eq!(
            sqlite_table_row_count(&path, "projection_mutations").await,
            mutations_before
        );
        assert_eq!(
            sqlite_table_indexes(&path, "projection_mutations").await,
            expected_indexes
        );
    }

    #[tokio::test]
    async fn projection_witness_v2_schema_with_extra_partial_index_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("extra-partial-index.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x54; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        let commits_before = sqlite_integrity_commit_count(&path).await;
        let mut expected_indexes = sqlite_table_indexes(&path, "projection_mutations").await;
        expected_indexes.push("projection_mutations_attack_partial".to_string());
        expected_indexes.sort();
        mutate_projection_mutations_indexes(
            &path,
            &[
                "CREATE INDEX projection_mutations_attack_partial ON projection_mutations(managed_mcp_id) WHERE status = 'started'",
            ],
        )
        .await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert_eq!(
            sqlite_table_indexes(&path, "projection_mutations").await,
            expected_indexes
        );
    }

    #[tokio::test]
    async fn projection_witness_v2_schema_with_altered_active_index_contract_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("altered-active-index.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x55; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        let commits_before = sqlite_integrity_commit_count(&path).await;
        let mutations_before = sqlite_table_row_count(&path, "projection_mutations").await;
        mutate_projection_mutations_indexes(
            &path,
            &[
                "DROP INDEX projection_mutations_active",
                "CREATE UNIQUE INDEX projection_mutations_active ON projection_mutations(managed_mcp_id COLLATE NOCASE DESC) WHERE status IN ('started','config_committed')",
            ],
        )
        .await;
        let expected_indexes = sqlite_table_indexes(&path, "projection_mutations").await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert_eq!(
            sqlite_table_row_count(&path, "projection_mutations").await,
            mutations_before
        );
        assert_eq!(
            sqlite_table_indexes(&path, "projection_mutations").await,
            expected_indexes
        );
    }

    #[tokio::test]
    async fn projection_witness_v2_schema_with_weakened_status_check_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("weak-status-check.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x3f; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        rewrite_projection_mutations_with_current_witness_shape(
            &path,
            r#"CREATE TABLE projection_mutations (
                mutation_id INTEGER PRIMARY KEY AUTOINCREMENT,
                managed_mcp_id TEXT NOT NULL,
                expected_revision INTEGER NOT NULL,
                previous_enabled INTEGER NOT NULL CHECK(previous_enabled IN (0,1)),
                desired_enabled INTEGER NOT NULL CHECK(desired_enabled IN (0,1)),
                status TEXT NOT NULL CHECK(status IN ('started','config_committed','committed','recovery_required','resolved','evil')),
                writer_runtime_id TEXT,
                writer_sink_id TEXT,
                writer_anchor_instance_id TEXT,
                writer_anchor_path_binding TEXT CHECK(writer_anchor_path_binding IS NULL OR length(writer_anchor_path_binding) = 64),
                writer_anchor_key_epoch INTEGER CHECK(writer_anchor_key_epoch IS NULL OR writer_anchor_key_epoch > 0),
                writer_anchor_sequence INTEGER CHECK(writer_anchor_sequence IS NULL OR writer_anchor_sequence >= 0),
                writer_anchor_root TEXT CHECK(writer_anchor_root IS NULL OR length(writer_anchor_root) = 64),
                writer_witness_version INTEGER CHECK(writer_witness_version IS NULL OR writer_witness_version = 2),
                writer_provider_id TEXT CHECK(writer_provider_id IS NULL OR writer_provider_id IN ('system-keyring','in-memory-test')),
                writer_binding_domain TEXT,
                writer_observed_state_digest TEXT CHECK(writer_observed_state_digest IS NULL OR length(writer_observed_state_digest) = 64),
                writer_commitment TEXT CHECK(writer_commitment IS NULL OR length(writer_commitment) = 64),
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            )"#,
            true,
        )
        .await;
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
    }

    #[tokio::test]
    async fn projection_witness_v2_schema_with_weakened_not_null_and_check_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("weak-not-null-check.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x40; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        rewrite_projection_mutations_with_current_witness_shape(
            &path,
            r#"CREATE TABLE projection_mutations (
                mutation_id INTEGER PRIMARY KEY AUTOINCREMENT,
                managed_mcp_id TEXT NOT NULL,
                expected_revision INTEGER NOT NULL,
                previous_enabled INTEGER NOT NULL CHECK(previous_enabled IN (0,1)),
                desired_enabled INTEGER NOT NULL CHECK(desired_enabled IN (0,1)),
                status TEXT NOT NULL CHECK(status IN ('started','config_committed','committed','recovery_required','resolved')),
                writer_runtime_id TEXT,
                writer_sink_id TEXT,
                writer_anchor_instance_id TEXT,
                writer_anchor_path_binding TEXT CHECK(writer_anchor_path_binding IS NULL OR length(writer_anchor_path_binding) = 64),
                writer_anchor_key_epoch INTEGER CHECK(writer_anchor_key_epoch IS NULL OR writer_anchor_key_epoch > 0),
                writer_anchor_sequence INTEGER CHECK(writer_anchor_sequence IS NULL OR writer_anchor_sequence >= 0),
                writer_anchor_root TEXT CHECK(writer_anchor_root IS NULL OR length(writer_anchor_root) = 64),
                writer_witness_version INTEGER CHECK(writer_witness_version IS NULL OR writer_witness_version = 2),
                writer_provider_id TEXT CHECK(writer_provider_id IS NULL OR writer_provider_id IN ('system-keyring','in-memory-test')),
                writer_binding_domain TEXT,
                writer_observed_state_digest TEXT,
                writer_commitment TEXT CHECK(writer_commitment IS NULL OR length(writer_commitment) = 64),
                created_at_ms INTEGER,
                updated_at_ms INTEGER NOT NULL
            )"#,
            true,
        )
        .await;
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
    }

    #[tokio::test]
    async fn future_schema_version_fails_closed_as_schema_too_new_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("future-schema.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x41; 32],
            database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        let commits_before = sqlite_integrity_commit_count(&path).await;
        mutate_schema_versions(
            &path,
            "INSERT INTO schema_version(version, applied_at_ms) VALUES (?, 0)",
            migrations::CURRENT_SCHEMA_VERSION + 1,
        )
        .await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::SchemaTooNew);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
    }

    #[tokio::test]
    async fn legacy_v20_source_schema_reopen_migrates_to_v21_and_unblocks_source_catalog_dao() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-v20-source-schema.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x42; 32],
            database_path_binding(&path).unwrap(),
        );
        create_v20_repository_fixture(&path, signer.clone()).await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1_i64..=20).collect::<Vec<_>>()
        );

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        let ledger_versions = sqlx::query_scalar::<_, i64>(
            "SELECT version FROM source_schema_ledger ORDER BY version",
        )
        .fetch_all(&repository.pool)
        .await
        .unwrap();
        assert_eq!(ledger_versions, vec![20, 21]);

        let schema_hash: String =
            sqlx::query_scalar("SELECT schema_hash FROM source_schema_fence WHERE fence_id = ?")
                .bind(crate::verified_source_catalog::schema::SOURCE_SCHEMA_FENCE_ID)
                .fetch_one(&repository.pool)
                .await
                .unwrap();
        assert_eq!(schema_hash.len(), 64);

        crate::verified_source_catalog::ensure_source_schema_compatible(&repository.pool)
            .await
            .unwrap();
        let dao = crate::verified_source_catalog::SourceCatalogDao::new(repository.pool.clone());
        assert!(dao
            .list_releases_by_source("missing-source")
            .await
            .unwrap()
            .is_empty());

        repository.close().await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn legacy_v20_source_schema_v21_reopen_failure_rolls_back_without_half_migration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("legacy-v20-source-schema-rollback.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x43; 32],
            database_path_binding(&path).unwrap(),
        );
        create_v20_repository_fixture(&path, signer.clone()).await;
        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &["CREATE INDEX source_releases_lookup ON source_releases(version)"],
        )
        .await;
        assert!(sqlite_schema_object_exists(&path, "index", "source_releases_lookup").await);
        assert!(sqlite_table_indexes(&path, "source_releases")
            .await
            .contains(&"source_releases_lookup".to_string()));
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let error =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RepositoryUnavailable);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1_i64..=20).collect::<Vec<_>>()
        );
        assert!(!sqlite_schema_object_exists(&path, "table", "source_schema_fence").await);
        assert!(!sqlite_schema_object_exists(&path, "table", "source_schema_ledger").await);
        assert!(!sqlite_schema_object_exists(&path, "index", "source_operations_status").await);
        assert!(sqlite_schema_object_exists(&path, "index", "source_releases_lookup").await);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);

        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &["DROP INDEX source_releases_lookup"],
        )
        .await;
        assert!(!sqlite_schema_object_exists(&path, "index", "source_releases_lookup").await);

        let reopened = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap();
        crate::verified_source_catalog::ensure_source_schema_compatible(&reopened.pool)
            .await
            .unwrap();
        reopened.close().await;
    }

    #[tokio::test]
    async fn legacy_v21_reopen_migrates_to_v23_and_unblocks_intake_candidate_dao() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-v21-intake-schema.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x44; 32],
            database_path_binding(&path).unwrap(),
        );
        create_v21_repository_fixture(&path, signer.clone()).await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1_i64..=21).collect::<Vec<_>>()
        );

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        let private_reference = repository
            .mint_intake_private_reference(
                crate::mcp_platform::intake::IntakePrivateReferenceKind::CatalogManifest,
            )
            .unwrap();
        let configuration_ref = repository
            .save_intake_configuration_ref(crate::mcp_platform::intake::SaveIntakeConfigurationRef {
                configuration_ref: "cfg_migration",
                descriptor: &crate::mcp_platform::intake::IntakeConfigurationDescriptor::CatalogPlanning {
                    source_id: "local_persistence".to_string(),
                    mcp_id: "fixture".to_string(),
                    version: "1.0.0".to_string(),
                },
                private_reference: Some(private_reference.as_str()),
                descriptor_digest: &"c".repeat(64),
                now_ms: 1,
            })
            .await
            .unwrap();
        let candidate = repository
            .save_intake_candidate(crate::mcp_platform::intake::SaveIntakeCandidate {
                candidate_id: "candidate_migration",
                submission_binding: &"d".repeat(64),
                source_facet: crate::mcp_platform::intake::IntakeSourceFacet::CatalogPlanning,
                transport: crate::mcp_platform::intake::IntakeTransport::CatalogReference,
                initial_state: crate::mcp_platform::intake::IntakeLifecycleState::Submitted,
                redacted_configuration_ref: &configuration_ref,
                descriptor_digest: &"c".repeat(64),
                now_ms: 2,
            })
            .await
            .unwrap();
        assert_eq!(
            candidate.lifecycle_state,
            crate::mcp_platform::intake::IntakeLifecycleState::Submitted
        );

        repository.close().await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert!(sqlite_schema_object_exists(&path, "table", "mcp_intake_candidates").await);
        assert!(
            sqlite_schema_object_exists(&path, "table", "mcp_intake_candidate_revisions").await
        );
        assert!(sqlite_schema_object_exists(&path, "table", "mcp_intake_binding_events").await);
        assert!(
            sqlite_schema_object_exists(&path, "index", "mcp_intake_candidate_revision_lookup")
                .await
        );
    }

    #[tokio::test]
    async fn legacy_v21_intake_reopen_failure_rolls_back_without_half_migration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("legacy-v21-intake-schema-rollback.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x45; 32],
            database_path_binding(&path).unwrap(),
        );
        create_v21_repository_fixture(&path, signer.clone()).await;
        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &["CREATE TABLE mcp_intake_candidates(dummy TEXT)"],
        )
        .await;
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let error =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RepositoryUnavailable);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1_i64..=21).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert!(
            !sqlite_schema_object_exists(&path, "table", "mcp_intake_configuration_refs").await
        );
        assert!(
            !sqlite_schema_object_exists(&path, "table", "mcp_intake_candidate_revisions").await
        );
        assert!(
            !sqlite_schema_object_exists(&path, "index", "mcp_intake_candidate_revision_lookup")
                .await
        );
        assert!(sqlite_schema_object_exists(&path, "table", "mcp_intake_candidates").await);

        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &["DROP TABLE mcp_intake_candidates"],
        )
        .await;
        let reopened = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap();
        assert_eq!(
            reopened
                .get_intake_candidate_by_submission_binding(&"e".repeat(64))
                .await
                .unwrap(),
            None
        );
        reopened.close().await;
    }

    #[tokio::test]
    async fn legacy_v27_governed_catalog_reopen_failure_rolls_back_without_half_migration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("legacy-v27-governed-catalog-schema-rollback.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x46; 32],
            database_path_binding(&path).unwrap(),
        );
        create_repository_fixture_with_version(&path, signer.clone(), 27).await;
        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &[r#"CREATE TABLE governed_catalog_sources(dummy TEXT)"#],
        )
        .await;
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let error =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RepositoryUnavailable);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1_i64..=27).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert!(sqlite_schema_object_exists(&path, "table", "governed_catalog_sources").await);
        assert!(!sqlite_schema_object_exists(&path, "table", "governed_catalog_documents").await);
        assert!(!sqlite_schema_object_exists(&path, "table", "governed_catalog_entries").await);
        assert!(
            !sqlite_schema_object_exists(&path, "index", "governed_catalog_entries_digest_lookup")
                .await
        );

        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &[r#"DROP TABLE governed_catalog_sources"#],
        )
        .await;

        let reopened = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap();
        reopened.close().await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert!(sqlite_schema_object_exists(&path, "table", "governed_catalog_sources").await);
        assert!(sqlite_schema_object_exists(&path, "table", "governed_catalog_documents").await);
        assert!(sqlite_schema_object_exists(&path, "table", "governed_catalog_entries").await);
        assert!(
            sqlite_schema_object_exists(&path, "index", "governed_catalog_entries_digest_lookup")
                .await
        );
    }

    #[tokio::test]
    async fn legacy_v29_refresh_registration_migrates_but_fails_closed_without_a_trust_anchor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("legacy-v29-refresh-registration-without-anchor.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x47; 32],
            database_path_binding(&path).unwrap(),
        );
        create_repository_fixture_with_version(&path, signer.clone(), 29).await;
        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &[
                r#"INSERT INTO governed_catalog_sources(
                        source_id, import_kind, source_ref_json, display_name, created_at_ms, updated_at_ms
                    ) VALUES (
                        'source-legacy-no-anchor',
                        'verified_source_catalog',
                        '{"kind":"verified_source_catalog","source_id":"source-legacy-no-anchor"}',
                        'Legacy source', 1, 1
                    )"#,
                r#"INSERT INTO governed_source_refresh_registrations(
                        source_id, transport_kind, endpoint, created_at_ms, updated_at_ms,
                        last_attempted_at_ms, last_refreshed_at_ms, last_result,
                        last_document_digest, last_error_code
                    ) VALUES (
                        'source-legacy-no-anchor', 'verified_source_bundle_v1',
                        'https://catalog.example.com/source-legacy-no-anchor.bundle',
                        1, 1, NULL, NULL, 'idle', NULL, NULL
                    )"#,
            ],
        )
        .await;

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(
            repository
                .get_governed_source_refresh_registration("source-legacy-no-anchor")
                .await
                .unwrap()
                .last_result,
            GovernedSourceRefreshResultState::Idle
        );
        let anchor_error = repository
            .get_governed_source_refresh_trust_anchor("source-legacy-no-anchor")
            .await
            .unwrap_err();
        assert_eq!(
            anchor_error.code(),
            McpPlatformErrorCode::OperationNotSupported
        );
        let reprovision_error = repository
            .save_governed_source_refresh_registration(SaveGovernedSourceRefreshRegistration {
                source_id: "source-legacy-no-anchor",
                transport_kind: GovernedSourceRefreshTransportKind::VerifiedSourceBundleV1,
                endpoint: "https://catalog.example.com/source-legacy-no-anchor.bundle",
                now_ms: 2,
            })
            .await
            .unwrap_err();
        assert_eq!(
            reprovision_error.code(),
            McpPlatformErrorCode::OperationNotSupported
        );
        repository.close().await;
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn system_open_path_completes_guarded_v14_v15_v31_and_v32_migrations_to_current() {
        for version in [14_i64, 15, 31, 32] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory
                .path()
                .join(format!("system-guarded-v{version}.db"));
            let (signer, path_binding, checkpoint) = create_system_keyring_s0_state(&path).await;

            if version >= 15 {
                signer
                    .ensure_schema_compatibility_fence(
                        &checkpoint,
                        &expected_v15_schema_fence_pending(),
                    )
                    .unwrap();
                let pool = SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_with(
                        SqliteConnectOptions::new()
                            .filename(&path)
                            .create_if_missing(false)
                            .foreign_keys(true)
                            .busy_timeout(Duration::from_secs(30))
                            .journal_mode(SqliteJournalMode::Wal),
                    )
                    .await
                    .unwrap();
                let v15_checkpoint =
                    migrate_v15_with_internal_integrity_commit(&pool, signer.as_ref(), &checkpoint)
                        .await
                        .unwrap();
                pool.close().await;
                signer.publish(Some(&checkpoint), &v15_checkpoint).unwrap();
                signer
                    .ensure_schema_compatibility_fence(
                        &v15_checkpoint,
                        &expected_v15_schema_fence_active(),
                    )
                    .unwrap();
                if version >= 31 {
                    let pool = SqlitePoolOptions::new()
                        .max_connections(1)
                        .connect_with(
                            SqliteConnectOptions::new()
                                .filename(&path)
                                .create_if_missing(false)
                                .foreign_keys(true)
                                .busy_timeout(Duration::from_secs(30))
                                .journal_mode(SqliteJournalMode::Wal),
                        )
                        .await
                        .unwrap();
                    migrate_through_version_with_integrity_commit(
                        &pool,
                        signer.as_ref(),
                        Some(&path_binding),
                        version,
                    )
                    .await
                    .unwrap();
                    pool.close().await;
                }
            }

            let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
            repository.close().await;
            assert_eq!(
                sqlite_schema_versions(&path).await,
                (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
            );
            let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
            reopened.close().await;
            assert_eq!(
                sqlite_schema_versions(&path).await,
                (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
            );
            integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding).unwrap();
        }
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn staged_v15_migration_reopen_from_s1_completes_to_active_guard() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("staged-s1-reopen.db");
        let (signer, path_binding, checkpoint) = create_system_keyring_s0_state(&path).await;
        let raw_s0 = integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();
        assert!(raw_s0.contains("\"format_version\":2"));
        assert!(!raw_s0.contains("schema_fence"));
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );

        signer
            .ensure_schema_compatibility_fence(&checkpoint, &expected_v15_schema_fence_pending())
            .unwrap();
        let raw_s1 = integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();
        assert!(raw_s1.contains("\"phase\":\"pending\""));
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );

        let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        reopened.close().await;

        let raw_s3 = integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();
        let finalized = integrity::system_signer(&path_binding, false)
            .checkpoint()
            .unwrap()
            .unwrap();
        assert_eq!(finalized.sequence, checkpoint.sequence + 1);
        assert!(raw_s3.contains("\"phase\":\"active\""));
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, 2);
        integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding).unwrap();
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn staged_v15_migration_reopen_from_post_migration_pending_completes_to_active_guard() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("staged-s2-reopen.db");
        let (signer, path_binding, checkpoint) = create_system_keyring_s0_state(&path).await;
        signer
            .ensure_schema_compatibility_fence(&checkpoint, &expected_v15_schema_fence_pending())
            .unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(false)
                    .foreign_keys(true)
                    .busy_timeout(Duration::from_secs(30))
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await
            .unwrap();
        let migrated_checkpoint =
            migrate_v15_with_internal_integrity_commit(&pool, signer.as_ref(), &checkpoint)
                .await
                .unwrap();
        pool.close().await;

        let raw_s2 = integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();
        assert!(raw_s2.contains("\"phase\":\"pending\""));
        assert_eq!(signer.checkpoint().unwrap().unwrap(), checkpoint);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );

        let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        reopened.close().await;

        let finalized = integrity::system_signer(&path_binding, false)
            .checkpoint()
            .unwrap()
            .unwrap();
        let raw_s3 = integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();
        assert_eq!(finalized, migrated_checkpoint);
        assert!(raw_s3.contains("\"phase\":\"active\""));
        assert_eq!(sqlite_integrity_commit_count(&path).await, 2);
        integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding).unwrap();
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn staged_guard_publish_failure_leaves_database_in_s0_without_starting_v15_migration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("staged-publish-failure.db");
        let (_signer, path_binding, _checkpoint) = create_system_keyring_s0_state(&path).await;
        let raw_before = integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();
        let commits_before = sqlite_integrity_commit_count(&path).await;
        let _guard = integrity::fail_next_system_signer_schema_fence_publish(path_binding.clone());

        let error = SqliteMcpPlatformRepository::open_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert_eq!(
            integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap(),
            raw_before
        );

        let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        reopened.close().await;
        assert!(integrity::read_raw_system_anchor_for_testing(&path_binding)
            .unwrap()
            .contains("\"phase\":\"active\""));
        integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding).unwrap();
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn existing_v15_v2_guard_adoption_recovers_from_pending_guard_crash() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("existing-v15-v2-adoption.db");
        let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        repository.close().await;
        let path_binding = database_path_binding(&path).unwrap();
        let raw_active = integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();
        let downgraded_guard = downgrade_system_guard_to_v2(&raw_active);
        integrity::write_raw_system_anchor_for_testing(&path_binding, &downgraded_guard).unwrap();
        let commits_before = sqlite_integrity_commit_count(&path).await;
        let _guard = integrity::fail_next_system_signer_schema_fence_publish_then_error(
            path_binding.clone(),
        );

        let error = SqliteMcpPlatformRepository::open_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        let pending_raw = integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();
        assert!(pending_raw.contains("\"phase\":\"pending\""));
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );

        let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        reopened.close().await;
        assert!(integrity::read_raw_system_anchor_for_testing(&path_binding)
            .unwrap()
            .contains("\"phase\":\"active\""));
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding).unwrap();
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn concurrent_reopen_from_s1_serializes_without_unsafe_interleaving() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("staged-concurrent-reopen.db");
        let (signer, path_binding, checkpoint) = create_system_keyring_s0_state(&path).await;
        signer
            .ensure_schema_compatibility_fence(&checkpoint, &expected_v15_schema_fence_pending())
            .unwrap();

        let (first, second) = tokio::join!(
            SqliteMcpPlatformRepository::open_path(&path),
            SqliteMcpPlatformRepository::open_path(&path)
        );
        let first = first.unwrap();
        let second = second.unwrap();
        first.close().await;
        second.close().await;

        assert!(integrity::read_raw_system_anchor_for_testing(&path_binding)
            .unwrap()
            .contains("\"phase\":\"active\""));
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, 2);
        integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding).unwrap();
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn v15_schema_guard_is_minted_and_downshift_rejection_does_not_advance_anchor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("guarded-downshift.db");
        let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        repository.close().await;
        let path_binding = database_path_binding(&path).unwrap();
        let raw_guard = integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();
        assert!(raw_guard.contains("\"format_version\":3"));
        assert!(raw_guard.contains("\"minimum_schema_version\":15"));
        assert!(raw_guard.contains("\"phase\":\"active\""));
        assert!(raw_guard
            .contains("\"projection_mutations_shape\":\"projection_mutations_witness_v2_v15\""));

        let commits_before = sqlite_integrity_commit_count(&path).await;
        mutate_schema_versions(&path, "DELETE FROM schema_version WHERE version = ?", 15).await;
        let error = SqliteMcpPlatformRepository::open_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert_eq!(
            integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap(),
            raw_guard
        );

        let mut downgraded_guard = serde_json::from_str::<serde_json::Value>(&raw_guard).unwrap();
        downgraded_guard = serde_json::from_str(&downgrade_system_guard_to_v2(&raw_guard)).unwrap();
        integrity::write_raw_system_anchor_for_testing(
            &path_binding,
            &serde_json::to_string(&downgraded_guard).unwrap(),
        )
        .unwrap();
        mutate_schema_versions(&path, "DELETE FROM schema_version WHERE version = ?", 15).await;

        let error = SqliteMcpPlatformRepository::open_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert_eq!(
            integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap(),
            serde_json::to_string(&downgraded_guard).unwrap()
        );
        let _ = integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding);
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn missing_v15_schema_guard_and_downshift_fail_closed_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("guard-missing.db");
        let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        repository.close().await;
        let path_binding = database_path_binding(&path).unwrap();
        let commits_before = sqlite_integrity_commit_count(&path).await;

        integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding).unwrap();
        mutate_schema_versions(&path, "DELETE FROM schema_version WHERE version = ?", 15).await;

        let error = SqliteMcpPlatformRepository::open_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn malformed_v15_schema_guard_fails_closed_without_rewriting_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("guard-malformed.db");
        let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
        repository.close().await;
        let path_binding = database_path_binding(&path).unwrap();
        let raw_guard = integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let mut malformed = serde_json::from_str::<serde_json::Value>(&raw_guard).unwrap();
        malformed["schema_fence"]["projection_mutations_shape"] =
            serde_json::json!("unknown-shape");
        integrity::write_raw_system_anchor_for_testing(
            &path_binding,
            &serde_json::to_string(&malformed).unwrap(),
        )
        .unwrap();

        let error = SqliteMcpPlatformRepository::open_path(&path)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert_eq!(
            integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap(),
            serde_json::to_string(&malformed).unwrap()
        );
        let _ = integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding);
    }

    #[cfg(feature = "system-keyring")]
    #[tokio::test]
    async fn strict_v15_schema_guard_payloads_fail_closed_without_writes_or_anchor_advance() {
        let guarded_cases = [
            "schema-fence-phase-missing",
            "schema-fence-phase-null",
            "schema-fence-phase-unknown",
            "schema-fence-phase-number",
            "schema-fence-extra-field",
            "provider-swapped-to-in-memory",
            "identity-path-binding-partial",
            "identity-path-binding-replaced",
            "identity-key-epoch-replaced",
        ];

        for (index, case) in guarded_cases.into_iter().enumerate() {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(format!("guard-strict-{index}.db"));
            let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
            repository.close().await;
            let path_binding = database_path_binding(&path).unwrap();
            let raw_guard = integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap();
            let valid_payload = serde_json::from_str::<serde_json::Value>(&raw_guard).unwrap();
            let payload = invalid_system_anchor_payloads(&valid_payload, &raw_guard)
                .into_iter()
                .find(|(name, _)| *name == case)
                .unwrap()
                .1;
            let schema_versions_before = sqlite_schema_versions(&path).await;
            let commits_before = sqlite_integrity_commit_count(&path).await;

            integrity::write_raw_system_anchor_for_testing(&path_binding, &payload).unwrap();

            let error = SqliteMcpPlatformRepository::open_path(&path)
                .await
                .unwrap_err();
            assert_eq!(
                error.code(),
                McpPlatformErrorCode::IntegrityError,
                "case {case} failed with unexpected error"
            );
            assert_eq!(sqlite_schema_versions(&path).await, schema_versions_before);
            assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
            assert_eq!(
                integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap(),
                payload
            );

            let retry = SqliteMcpPlatformRepository::open_path(&path)
                .await
                .unwrap_err();
            assert_eq!(retry.code(), McpPlatformErrorCode::IntegrityError);
            assert_eq!(sqlite_schema_versions(&path).await, schema_versions_before);
            assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
            assert_eq!(
                integrity::read_raw_system_anchor_for_testing(&path_binding).unwrap(),
                payload
            );
            let _ = integrity::clear_system_anchor_for_trusted_reenrollment(&path_binding);
        }
    }

    #[tokio::test]
    async fn forged_projection_mutation_never_reaches_recovery_consumption() {
        let signer = InMemoryIntegritySigner::new_for_testing([0x33; 32]);
        let repository =
            SqliteMcpPlatformRepository::open_url_with_integrity_signer("sqlite::memory:", signer)
                .await
                .unwrap();
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "mutation-managed".to_string(),
                    mcp_id: "mutation.example".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO projection_mutations(managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,created_at_ms,updated_at_ms) VALUES ('mutation-managed',0,0,1,'started',2,2)",
        )
        .execute(&repository.pool)
        .await
        .unwrap();

        let error = repository
            .list_pending_projection_mutations()
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[tokio::test]
    async fn forged_projection_proof_is_rejected_before_receipt_issuance() {
        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            Arc::new(InMemoryIntegritySigner::new_for_testing([0x34; 32])),
        )
        .await
        .unwrap();
        let ports = test_ports_with_projection_adapter_version("1");
        let authority =
            crate::mcp_platform::projection_runtime::bootstrap_debug_repository_authority(
                &repository,
                &ports,
            );
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "forged-proof-managed".to_string(),
                    mcp_id: "forged-proof.example".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        let inventory = repository
            .get_managed_inventory("forged-proof-managed")
            .await
            .unwrap();
        let mutation = repository
            .begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                "forged-proof-managed",
                inventory.managed.revision,
                false,
                2,
            )
            .await
            .unwrap();
        let authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            )
            .await
            .unwrap();
        let digest = crate::mcp_platform::lifecycle::observed_projection_digest(
            authority.sink_identity(),
            authorization
                .mutation()
                .writer_runtime_id
                .as_deref()
                .unwrap(),
            None,
        )
        .unwrap();
        let error = authority
            .issue_sink_commit_receipt(
                &authorization,
                forged_projection_proof(&authorization, digest),
            )
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[tokio::test]
    async fn malformed_projection_witness_values_fail_closed_across_begin_authorize_complete_recovery_and_resolve(
    ) {
        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            Arc::new(InMemoryIntegritySigner::new_for_testing([0x3a; 32])),
        )
        .await
        .unwrap();
        let ports = test_ports_with_projection_adapter_version("1");
        let authority =
            crate::mcp_platform::projection_runtime::bootstrap_debug_repository_authority(
                &repository,
                &ports,
            );

        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "witness-managed".to_string(),
                    mcp_id: "witness.example".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        let inventory = repository
            .get_managed_inventory("witness-managed")
            .await
            .unwrap();
        let mutation = repository
            .begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                "witness-managed",
                inventory.managed.revision,
                false,
                2,
            )
            .await
            .unwrap();
        sqlx::query("UPDATE projection_mutations SET writer_binding_domain = 'evil.domain' WHERE mutation_id = ?")
            .bind(mutation.mutation_id)
            .execute(&repository.pool)
            .await
            .unwrap();
        let begin_error = repository
            .begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                "witness-managed",
                inventory.managed.revision,
                false,
                3,
            )
            .await
            .unwrap_err();
        assert_eq!(begin_error.code(), McpPlatformErrorCode::IntegrityError);
        let authorize_error = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            )
            .await
            .unwrap_err();
        assert_eq!(authorize_error.code(), McpPlatformErrorCode::IntegrityError);
        let recovery_error = repository
            .mark_projection_mutation_recovery_required_authorized(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
                4,
            )
            .await
            .unwrap_err();
        assert_eq!(recovery_error.code(), McpPlatformErrorCode::IntegrityError);

        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            Arc::new(InMemoryIntegritySigner::new_for_testing([0x3b; 32])),
        )
        .await
        .unwrap();
        let ports = test_ports();
        let authority =
            crate::mcp_platform::projection_runtime::bootstrap_debug_repository_authority(
                &repository,
                &ports,
            );
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "witness-complete".to_string(),
                    mcp_id: "witness.complete".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        let inventory = repository
            .get_managed_inventory("witness-complete")
            .await
            .unwrap();
        let mutation = repository
            .begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                "witness-complete",
                inventory.managed.revision,
                false,
                2,
            )
            .await
            .unwrap();
        let authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            )
            .await
            .unwrap();
        repository
            .mark_projection_config_committed_authorized(
                authority.write_capability(),
                &authority,
                &authorization,
                3,
            )
            .await
            .unwrap();
        let completion_authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            )
            .await
            .unwrap();
        let receipt = test_projection_receipt(
            &authority,
            &completion_authorization,
            crate::mcp_platform::lifecycle::observed_projection_digest(
                authority.sink_identity(),
                completion_authorization
                    .mutation()
                    .writer_runtime_id
                    .as_deref()
                    .unwrap(),
                None,
            )
            .unwrap(),
        );
        sqlx::query("UPDATE projection_mutations SET writer_provider_id = 'system-keyring' WHERE mutation_id = ?")
            .bind(mutation.mutation_id)
            .execute(&repository.pool)
            .await
            .unwrap();
        let complete_error = repository
            .complete_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                &completion_authorization,
                &receipt,
                4,
            )
            .await
            .unwrap_err();
        assert_eq!(
            complete_error.code(),
            McpPlatformErrorCode::ProjectionWitnessExpired
        );

        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            Arc::new(InMemoryIntegritySigner::new_for_testing([0x3c; 32])),
        )
        .await
        .unwrap();
        let ports = test_ports();
        let authority =
            crate::mcp_platform::projection_runtime::bootstrap_debug_repository_authority(
                &repository,
                &ports,
            );
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "witness-resolve".to_string(),
                    mcp_id: "witness.resolve".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        let inventory = repository
            .get_managed_inventory("witness-resolve")
            .await
            .unwrap();
        let mutation = repository
            .begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                "witness-resolve",
                inventory.managed.revision,
                false,
                2,
            )
            .await
            .unwrap();
        sqlx::query("UPDATE projection_mutations SET writer_witness_version = 1, status = 'recovery_required' WHERE mutation_id = ?")
            .bind(mutation.mutation_id)
            .execute(&repository.pool)
            .await
            .unwrap();
        let authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            )
            .await
            .unwrap_err();
        assert_eq!(authorization.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[tokio::test]
    async fn forged_profile_revision_is_rejected_before_read_or_application() {
        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            InMemoryIntegritySigner::new_for_testing([0x34; 32]),
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mcp_profiles(profile_id,name,description,revision,archived,created_at_ms,updated_at_ms) VALUES ('forged-profile','forged','',1,0,1,1)",
        )
        .execute(&repository.pool)
        .await
        .unwrap();

        let error = repository.get_profile("forged-profile").await.unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[tokio::test]
    async fn legacy_step_binding_is_rejected_before_execution_refresh_and_recovery() {
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x35; 32]));
        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            signer.clone(),
        )
        .await
        .unwrap();
        let plan = save_manifest_and_plan(
            &repository,
            REMOTE,
            TrustTier::Official,
            "legacy-step-plan",
            "legacy-step-key",
        )
        .await;
        let task = create_task(
            &repository,
            "legacy-step-plan",
            &plan,
            "legacy-step-task",
            "legacy-step-task-key",
        )
        .await;
        let task = advance_to_running(&repository, task, 40).await;
        repository
            .add_task_step(
                &task.task_id,
                0,
                "legacy-step-token",
                &CompensationDescriptor::NoCompensation,
                "tester",
                task.revision,
                43,
            )
            .await
            .unwrap();
        repository
            .transition_task_step(StepTransition {
                task_id: &task.task_id,
                ordinal: 0,
                owner_id: "tester",
                expected_task_revision: task.revision,
                expected_status: TaskStepStatus::NotStarted,
                next_status: TaskStepStatus::Started,
                evidence: None,
                actor: "tester",
                now_ms: 44,
            })
            .await
            .unwrap();
        let task = repository.get_task(&task.task_id).await.unwrap();
        let step = repository
            .list_task_steps(&task.task_id)
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let compensation_json = sqlx::query_scalar::<_, String>(
            "SELECT compensation_json FROM task_steps WHERE task_id = ? AND ordinal = 0",
        )
        .bind(&task.task_id)
        .fetch_one(&repository.pool)
        .await
        .unwrap();
        let legacy_mac = signer
            .sign(
                "task-step-compensation",
                &legacy_task_step_binding_payload(&task, &step, &compensation_json),
            )
            .unwrap();
        let tampered_evidence = serde_json::to_string(&StepEvidence::HealthObserved {
            result_code: "tampered".to_string(),
        })
        .unwrap();
        sqlx::query(
            "UPDATE task_steps SET status = 'committed', evidence_json = ? WHERE task_id = ? AND ordinal = 0",
        )
        .bind(&tampered_evidence)
        .bind(&task.task_id)
        .execute(&repository.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE task_step_compensation_bindings SET mac = ? WHERE task_id = ? AND ordinal = 0",
        )
        .bind(&legacy_mac)
        .bind(&task.task_id)
        .execute(&repository.pool)
        .await
        .unwrap();

        let execution_error = repository
            .authorize_execution(&task.task_id, "tester", 45)
            .await
            .unwrap_err();
        assert_eq!(execution_error.code(), McpPlatformErrorCode::IntegrityError);
        let refresh_error = repository
            .transition_task_step(StepTransition {
                task_id: &task.task_id,
                ordinal: 0,
                owner_id: "tester",
                expected_task_revision: task.revision,
                expected_status: TaskStepStatus::Started,
                next_status: TaskStepStatus::Committed,
                evidence: Some(&StepEvidence::HealthObserved {
                    result_code: "ok".to_string(),
                }),
                actor: "tester",
                now_ms: 46,
            })
            .await
            .unwrap_err();
        assert_eq!(refresh_error.code(), McpPlatformErrorCode::IntegrityError);

        sqlx::query(
            "UPDATE tasks SET heartbeat_at_ms = 0, updated_at_ms = 0, lease_expires_at_ms = 0 WHERE task_id = ?",
        )
        .bind(&task.task_id)
        .execute(&repository.pool)
        .await
        .unwrap();
        let recovery_error = repository
            .recover_stale_tasks(1, "worker-b", 2)
            .await
            .unwrap_err();
        assert_eq!(recovery_error.code(), McpPlatformErrorCode::IntegrityError);

        let stored_mac = sqlx::query_scalar::<_, String>(
            "SELECT mac FROM task_step_compensation_bindings WHERE task_id = ? AND ordinal = 0",
        )
        .bind(&task.task_id)
        .fetch_one(&repository.pool)
        .await
        .unwrap();
        assert_eq!(stored_mac, legacy_mac);
    }

    #[tokio::test]
    async fn register_managed_mcp_id_mismatch_is_rejected() {
        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            InMemoryIntegritySigner::new_for_testing([0x38; 32]),
        )
        .await
        .unwrap();
        let (manifest, plan) = plan(REMOTE, TrustTier::Official);
        repository
            .save_manifest(&ManifestRecord {
                verified: manifest,
                proof: ManifestProof::LocalBytes,
                trust_tier: TrustTier::Official,
                source_metadata: ManifestSourceMetadata::local_persistence(),
                created_at_ms: 10,
            })
            .await
            .unwrap();
        repository
            .save_plan(SavePlan {
                plan_id: "register-mismatch-plan",
                idempotency_key: "register-mismatch-key",
                plan: &plan,
                target: &PlanTarget {
                    managed_mcp_id: Some("managed-client-supplied".to_string()),
                    mcp_id: plan.manifest_id().to_string(),
                    version: plan.manifest_version().to_string(),
                    installation_scope: None,
                    source_context: None,
                },
                policy_evidence: plan.policy(),
                confirmation_evidence: &ConfirmationEvidence::Pending,
                expires_at_ms: 10_000,
                created_at_ms: 20,
                actor: "tester",
            })
            .await
            .unwrap();

        let error = repository
            .create_task(CreateTask {
                task_id: "register-mismatch-task",
                plan_id: "register-mismatch-plan",
                plan_digest: plan.plan_digest(),
                operation: crate::mcp_platform::task::TaskOperation::Register,
                idempotency_key: "register-mismatch-task-key",
                actor: "tester",
                now_ms: 30,
                adapter_evidence: None,
                rollback_evidence: None,
            })
            .await
            .unwrap_err();

        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[tokio::test]
    async fn authorize_projection_mutation_loads_v27_manifest_provenance_for_active_projection() {
        let (_directory, repository) =
            open_temp_repository_with_test_signer("projection-v27.db", [0x96; 32]).await;
        let ports = test_ports();
        let authority =
            crate::mcp_platform::projection_runtime::bootstrap_debug_repository_authority(
                &repository,
                &ports,
            );
        let source_metadata = v27_test_source_metadata();
        let plan = save_managed_manifest_and_plan_with_source_metadata(
            &repository,
            NPM,
            TrustTier::Official,
            "projection-v27-plan",
            "projection-v27-key",
            "user",
            source_metadata.clone(),
        )
        .await;
        let (_task, managed_mcp_id, projection) = stage_active_owned_projection(
            &repository,
            &plan,
            "projection-v27-plan",
            "projection-v27-task",
            "projection-v27-task-key",
            "tester",
            40,
        )
        .await;
        let inventory = repository
            .get_managed_inventory(&managed_mcp_id)
            .await
            .unwrap();

        let mutation = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            repository.begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                &managed_mcp_id,
                inventory.managed.revision,
                false,
                60,
            ),
        )
        .await
        .expect("begin_projection_mutation_authorized must not self-deadlock")
        .unwrap();

        let authorization = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            repository.authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            ),
        )
        .await
        .expect("authorize_projection_mutation must not self-deadlock")
        .unwrap();

        assert_eq!(
            authorization
                .projection()
                .and_then(|record| record.manifest_digest.as_deref()),
            Some(plan.manifest_digest())
        );
        assert_eq!(
            authorization
                .projection()
                .and_then(|record| record.owner_task_id.as_deref()),
            Some("projection-v27-task")
        );
        assert_eq!(
            authorization.projection().map(|record| &record.link_key),
            Some(&projection.link_key)
        );
        assert_eq!(
            authorization
                .manifest()
                .map(|manifest| &manifest.source_metadata),
            Some(&source_metadata)
        );
        assert_eq!(
            authorization.manifest().map(|manifest| manifest.trust_tier),
            Some(TrustTier::Official)
        );
    }

    #[tokio::test]
    async fn mismatched_projection_writer_binding_remains_fail_closed_for_authorize_and_recovery() {
        let (_directory, repository) =
            open_temp_repository_with_test_signer("projection-mismatch.db", [0x98; 32]).await;
        let ports = test_ports();
        let authority =
            crate::mcp_platform::projection_runtime::bootstrap_debug_repository_authority(
                &repository,
                &ports,
            );
        let plan = save_managed_manifest_and_plan_with_source_metadata(
            &repository,
            NPM,
            TrustTier::Official,
            "projection-mismatch-plan",
            "projection-mismatch-key",
            "user",
            v27_test_source_metadata(),
        )
        .await;
        let (_task, managed_mcp_id, _projection) = stage_active_owned_projection(
            &repository,
            &plan,
            "projection-mismatch-plan",
            "projection-mismatch-task",
            "projection-mismatch-task-key",
            "tester",
            80,
        )
        .await;
        let inventory = repository
            .get_managed_inventory(&managed_mcp_id)
            .await
            .unwrap();
        let mutation = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            repository.begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                &managed_mcp_id,
                inventory.managed.revision,
                false,
                100,
            ),
        )
        .await
        .expect("begin_projection_mutation_authorized must not self-deadlock")
        .unwrap();
        sqlx::query("UPDATE projection_mutations SET writer_provider_id = 'system-keyring' WHERE mutation_id = ?")
            .bind(mutation.mutation_id)
            .execute(&repository.pool)
            .await
            .unwrap();

        let authorize_error = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            repository.authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            ),
        )
        .await
        .expect("authorize_projection_mutation must fail closed instead of self-deadlocking")
        .unwrap_err();
        assert_eq!(authorize_error.code(), McpPlatformErrorCode::IntegrityError);

        let recovery_error = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            repository.mark_projection_mutation_recovery_required_authorized(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
                101,
            ),
        )
        .await
        .expect(
            "mark_projection_mutation_recovery_required_authorized must fail closed instead of self-deadlocking",
        )
        .unwrap_err();
        assert_eq!(recovery_error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[tokio::test]
    async fn benign_projection_retry_and_read_do_not_expire_authorization() {
        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            Arc::new(InMemoryIntegritySigner::new_for_testing([0x37; 32])),
        )
        .await
        .unwrap();
        let ports = test_ports();
        let authority =
            crate::mcp_platform::projection_runtime::bootstrap_debug_repository_authority(
                &repository,
                &ports,
            );
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "projection-benign".to_string(),
                    mcp_id: "projection.benign".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        let inventory = repository
            .get_managed_inventory("projection-benign")
            .await
            .unwrap();
        let mutation = repository
            .begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                "projection-benign",
                inventory.managed.revision,
                true,
                2,
            )
            .await
            .unwrap();
        let authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            )
            .await
            .unwrap();
        let retry = repository
            .begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                "projection-benign",
                inventory.managed.revision,
                true,
                3,
            )
            .await
            .unwrap();
        assert_eq!(retry.mutation_id, mutation.mutation_id);
        assert_eq!(
            repository
                .list_pending_projection_mutations()
                .await
                .unwrap()
                .len(),
            1
        );

        repository
            .mark_projection_config_committed_authorized(
                authority.write_capability(),
                &authority,
                &authorization,
                4,
            )
            .await
            .unwrap();
        let completion_authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            )
            .await
            .unwrap();
        let retry_after_mark = repository
            .begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                "projection-benign",
                inventory.managed.revision,
                true,
                5,
            )
            .await
            .unwrap();
        assert_eq!(retry_after_mark.mutation_id, mutation.mutation_id);
        let receipt = test_projection_receipt(
            &authority,
            &completion_authorization,
            crate::mcp_platform::lifecycle::observed_projection_digest(
                authority.sink_identity(),
                completion_authorization
                    .mutation()
                    .writer_runtime_id
                    .as_deref()
                    .unwrap(),
                None,
            )
            .unwrap(),
        );
        repository
            .complete_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                &completion_authorization,
                &receipt,
                6,
            )
            .await
            .unwrap();

        let inventory = repository
            .get_managed_inventory("projection-benign")
            .await
            .unwrap();
        let mutation = repository
            .begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                "projection-benign",
                inventory.managed.revision,
                false,
                7,
            )
            .await
            .unwrap();
        let authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            )
            .await
            .unwrap();
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "projection-benign-other".to_string(),
                    mcp_id: "projection.benign.other".to_string(),
                    installation_scope: "user".to_string(),
                },
                8,
            )
            .await
            .unwrap();
        let mark_error = repository
            .mark_projection_config_committed_authorized(
                authority.write_capability(),
                &authority,
                &authorization,
                9,
            )
            .await
            .unwrap_err();
        assert_eq!(
            mark_error.code(),
            McpPlatformErrorCode::ProjectionWitnessExpired
        );
    }

    #[tokio::test]
    async fn projection_authorization_expires_on_any_anchor_change() {
        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            Arc::new(InMemoryIntegritySigner::new_for_testing([0x36; 32])),
        )
        .await
        .unwrap();
        let ports = test_ports();
        let authority =
            crate::mcp_platform::projection_runtime::bootstrap_debug_repository_authority(
                &repository,
                &ports,
            );
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "projection-managed".to_string(),
                    mcp_id: "projection.example".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        let inventory = repository
            .get_managed_inventory("projection-managed")
            .await
            .unwrap();
        let mark_mutation = repository
            .begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                "projection-managed",
                inventory.managed.revision,
                false,
                2,
            )
            .await
            .unwrap();
        let mark_authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mark_mutation.mutation_id,
            )
            .await
            .unwrap();
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "projection-other".to_string(),
                    mcp_id: "projection.other".to_string(),
                    installation_scope: "user".to_string(),
                },
                3,
            )
            .await
            .unwrap();
        let mark_error = repository
            .mark_projection_config_committed_authorized(
                authority.write_capability(),
                &authority,
                &mark_authorization,
                4,
            )
            .await
            .unwrap_err();
        assert_eq!(
            mark_error.code(),
            McpPlatformErrorCode::ProjectionWitnessExpired
        );
        let mark_status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM projection_mutations WHERE mutation_id = ?",
        )
        .bind(mark_mutation.mutation_id)
        .fetch_one(&repository.pool)
        .await
        .unwrap();
        assert_eq!(mark_status, "started");

        let inventory = repository
            .get_managed_inventory("projection-managed")
            .await
            .unwrap();
        let complete_mutation = repository
            .begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                "projection-managed",
                inventory.managed.revision,
                false,
                5,
            )
            .await
            .unwrap();
        let initial_authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                complete_mutation.mutation_id,
            )
            .await
            .unwrap();
        repository
            .mark_projection_config_committed_authorized(
                authority.write_capability(),
                &authority,
                &initial_authorization,
                6,
            )
            .await
            .unwrap();
        let completion_authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                complete_mutation.mutation_id,
            )
            .await
            .unwrap();
        let completion_receipt = test_projection_receipt(
            &authority,
            &completion_authorization,
            crate::mcp_platform::lifecycle::observed_projection_digest(
                authority.sink_identity(),
                completion_authorization
                    .mutation()
                    .writer_runtime_id
                    .as_deref()
                    .unwrap(),
                None,
            )
            .unwrap(),
        );
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "projection-third".to_string(),
                    mcp_id: "projection.third".to_string(),
                    installation_scope: "user".to_string(),
                },
                7,
            )
            .await
            .unwrap();
        let complete_error = repository
            .complete_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                &completion_authorization,
                &completion_receipt,
                8,
            )
            .await
            .unwrap_err();
        assert_eq!(
            complete_error.code(),
            McpPlatformErrorCode::ProjectionWitnessExpired
        );
        let complete_status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM projection_mutations WHERE mutation_id = ?",
        )
        .bind(complete_mutation.mutation_id)
        .fetch_one(&repository.pool)
        .await
        .unwrap();
        assert_eq!(complete_status, "config_committed");
    }

    #[tokio::test]
    async fn committed_projection_witness_is_reported_as_consumed() {
        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            Arc::new(InMemoryIntegritySigner::new_for_testing([0x5a; 32])),
        )
        .await
        .unwrap();
        let ports = test_ports();
        let authority =
            crate::mcp_platform::projection_runtime::bootstrap_debug_repository_authority(
                &repository,
                &ports,
            );
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "projection-consumed".to_string(),
                    mcp_id: "projection.consumed".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        let inventory = repository
            .get_managed_inventory("projection-consumed")
            .await
            .unwrap();
        let mutation = repository
            .begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                "projection-consumed",
                inventory.managed.revision,
                false,
                2,
            )
            .await
            .unwrap();
        let authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            )
            .await
            .unwrap();
        repository
            .mark_projection_config_committed_authorized(
                authority.write_capability(),
                &authority,
                &authorization,
                3,
            )
            .await
            .unwrap();
        let completion_authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            )
            .await
            .unwrap();
        let receipt = test_projection_receipt(
            &authority,
            &completion_authorization,
            crate::mcp_platform::lifecycle::observed_projection_digest(
                authority.sink_identity(),
                completion_authorization
                    .mutation()
                    .writer_runtime_id
                    .as_deref()
                    .unwrap(),
                None,
            )
            .unwrap(),
        );
        repository
            .complete_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                &completion_authorization,
                &receipt,
                4,
            )
            .await
            .unwrap();

        let consumed = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            )
            .await
            .unwrap_err();
        assert_eq!(
            consumed.code(),
            McpPlatformErrorCode::ProjectionWitnessConsumed
        );
    }

    #[tokio::test]
    async fn projection_receipts_reject_cross_authority_and_cross_repo_reuse() {
        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            Arc::new(InMemoryIntegritySigner::new_for_testing([0x51; 32])),
        )
        .await
        .unwrap();
        let ports = test_ports();
        let authority =
            crate::mcp_platform::projection_runtime::bootstrap_debug_repository_authority(
                &repository,
                &ports,
            );
        let other_ports = test_ports_with_projection_adapter_version("2");
        let other_authority =
            crate::mcp_platform::projection_runtime::bootstrap_debug_repository_authority(
                &repository,
                &other_ports,
            );
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "cross-managed".to_string(),
                    mcp_id: "cross.example".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        let inventory = repository
            .get_managed_inventory("cross-managed")
            .await
            .unwrap();
        let mutation = repository
            .begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                "cross-managed",
                inventory.managed.revision,
                false,
                2,
            )
            .await
            .unwrap();
        let authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            )
            .await
            .unwrap();
        repository
            .mark_projection_config_committed_authorized(
                authority.write_capability(),
                &authority,
                &authorization,
                3,
            )
            .await
            .unwrap();
        let completion_authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            )
            .await
            .unwrap();
        let receipt = test_projection_receipt(
            &authority,
            &completion_authorization,
            crate::mcp_platform::lifecycle::observed_projection_digest(
                authority.sink_identity(),
                completion_authorization
                    .mutation()
                    .writer_runtime_id
                    .as_deref()
                    .unwrap(),
                None,
            )
            .unwrap(),
        );
        let cross_authority = repository
            .complete_projection_mutation_authorized(
                other_authority.write_capability(),
                &other_authority,
                &completion_authorization,
                &receipt,
                4,
            )
            .await
            .unwrap_err();
        assert_eq!(
            cross_authority.code(),
            McpPlatformErrorCode::ProjectionWitnessExpired
        );

        let other_repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            Arc::new(InMemoryIntegritySigner::new_for_testing([0x52; 32])),
        )
        .await
        .unwrap();
        let other_ports = test_ports();
        let other_repository_authority =
            crate::mcp_platform::projection_runtime::bootstrap_debug_repository_authority(
                &other_repository,
                &other_ports,
            );
        other_repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "cross-managed".to_string(),
                    mcp_id: "cross.example".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        let other_inventory = other_repository
            .get_managed_inventory("cross-managed")
            .await
            .unwrap();
        let other_mutation = other_repository
            .begin_projection_mutation_authorized(
                other_repository_authority.write_capability(),
                &other_repository_authority,
                "cross-managed",
                other_inventory.managed.revision,
                false,
                2,
            )
            .await
            .unwrap();
        let other_authorization = other_repository
            .authorize_projection_mutation(
                other_repository_authority.write_capability(),
                &other_repository_authority,
                other_mutation.mutation_id,
            )
            .await
            .unwrap();
        other_repository
            .mark_projection_config_committed_authorized(
                other_repository_authority.write_capability(),
                &other_repository_authority,
                &other_authorization,
                3,
            )
            .await
            .unwrap();
        let other_completion_authorization = other_repository
            .authorize_projection_mutation(
                other_repository_authority.write_capability(),
                &other_repository_authority,
                other_mutation.mutation_id,
            )
            .await
            .unwrap();
        let cross_repo = other_repository
            .complete_projection_mutation_authorized(
                other_repository_authority.write_capability(),
                &other_repository_authority,
                &other_completion_authorization,
                &receipt,
                4,
            )
            .await
            .unwrap_err();
        assert_eq!(
            cross_repo.code(),
            McpPlatformErrorCode::ProjectionWitnessExpired
        );
    }

    #[tokio::test]
    async fn projection_receipts_fail_closed_after_authority_restart() {
        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            Arc::new(InMemoryIntegritySigner::new_for_testing([0x53; 32])),
        )
        .await
        .unwrap();
        let ports = test_ports();
        let authority =
            crate::mcp_platform::projection_runtime::bootstrap_debug_repository_authority(
                &repository,
                &ports,
            );
        repository
            .create_managed_mcp(
                &NewManagedMcp {
                    managed_mcp_id: "restart-managed".to_string(),
                    mcp_id: "restart.example".to_string(),
                    installation_scope: "user".to_string(),
                },
                1,
            )
            .await
            .unwrap();
        let inventory = repository
            .get_managed_inventory("restart-managed")
            .await
            .unwrap();
        let mutation = repository
            .begin_projection_mutation_authorized(
                authority.write_capability(),
                &authority,
                "restart-managed",
                inventory.managed.revision,
                false,
                2,
            )
            .await
            .unwrap();
        let authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            )
            .await
            .unwrap();
        repository
            .mark_projection_config_committed_authorized(
                authority.write_capability(),
                &authority,
                &authorization,
                3,
            )
            .await
            .unwrap();
        let completion_authorization = repository
            .authorize_projection_mutation(
                authority.write_capability(),
                &authority,
                mutation.mutation_id,
            )
            .await
            .unwrap();
        let digest = crate::mcp_platform::lifecycle::observed_projection_digest(
            authority.sink_identity(),
            completion_authorization
                .mutation()
                .writer_runtime_id
                .as_deref()
                .unwrap(),
            None,
        )
        .unwrap();
        let receipt = test_projection_receipt(&authority, &completion_authorization, digest);
        let restarted_authority =
            crate::mcp_platform::projection_runtime::bootstrap_debug_repository_authority(
                &repository,
                &ports,
            );
        let error = repository
            .complete_projection_mutation_authorized(
                restarted_authority.write_capability(),
                &restarted_authority,
                &completion_authorization,
                &receipt,
                4,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::ProjectionWitnessExpired);
    }

    #[tokio::test]
    async fn expired_worker_cannot_renew_and_is_fenced_after_recovery() {
        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            Arc::new(InMemoryIntegritySigner::new_for_testing([0x37; 32])),
        )
        .await
        .unwrap();
        let plan = save_manifest_and_plan(
            &repository,
            REMOTE,
            TrustTier::Official,
            "lease-plan",
            "lease-key",
        )
        .await;
        let task = create_task(
            &repository,
            "lease-plan",
            &plan,
            "lease-task",
            "lease-task-key",
        )
        .await;
        let claimed = advance_to_running(&repository, task, 40).await;
        let expired_at = claimed.lease_expires_at_ms.unwrap();

        let renew_error = repository
            .renew_task_lease(&claimed.task_id, "tester", expired_at, 30_000)
            .await
            .unwrap_err();
        assert_eq!(renew_error.code(), McpPlatformErrorCode::RevisionConflict);

        let recovered = repository
            .recover_stale_tasks(expired_at, "worker-b", expired_at + 1)
            .await
            .unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].task.owner_id.as_deref(), Some("worker-b"));

        let stale_auth_error = repository
            .authorize_execution(&claimed.task_id, "tester", expired_at + 2)
            .await
            .unwrap_err();
        assert_eq!(
            stale_auth_error.code(),
            McpPlatformErrorCode::RevisionConflict
        );
        let new_owner = repository
            .authorize_execution(&claimed.task_id, "worker-b", expired_at + 2)
            .await
            .unwrap();
        assert_eq!(new_owner.task.owner_id.as_deref(), Some("worker-b"));
    }

    #[tokio::test]
    async fn heartbeat_renewal_preserves_task_revision() {
        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            Arc::new(InMemoryIntegritySigner::new_for_testing([0x38; 32])),
        )
        .await
        .unwrap();
        let plan = save_manifest_and_plan(
            &repository,
            REMOTE,
            TrustTier::Official,
            "heartbeat-plan",
            "heartbeat-plan-key",
        )
        .await;
        let task = create_task(
            &repository,
            "heartbeat-plan",
            &plan,
            "heartbeat-task",
            "heartbeat-task-key",
        )
        .await;
        let claimed = advance_to_running(&repository, task, 40).await;
        let renewed = repository
            .renew_task_lease(&claimed.task_id, "tester", 42, 30_000)
            .await
            .unwrap();

        assert_eq!(renewed.revision, claimed.revision);
        let authorization = repository
            .authorize_execution(&claimed.task_id, "tester", 43)
            .await
            .unwrap();
        repository
            .validate_execution_authorization(&authorization, 44)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn v39_effect_fence_migrates_from_v38_and_reopens() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v39-from-v38.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x39; 32]));
        create_repository_fixture_with_version(&path, signer.clone(), 38).await;

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        let pool = existing_repository_pool(&path).await;
        let fence = sqlx::query(
            "SELECT scope,epoch,canonical_digest,created_at_ms FROM effect_fences WHERE scope='global'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(fence.try_get::<String, _>("scope").unwrap(), "global");
        assert_eq!(fence.try_get::<i64, _>("epoch").unwrap(), 0);
        assert_eq!(
            fence.try_get::<String, _>("canonical_digest").unwrap(),
            migrations::effect_fence_initial_digest()
        );
        assert_eq!(fence.try_get::<i64, _>("created_at_ms").unwrap(), 0);
        pool.close().await;

        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer).await;
        assert!(reopened.is_ok());
    }

    #[tokio::test]
    async fn v40_migrates_anchored_v39_effect_history_with_legacy_abandon_reason() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v40-from-v39-effect-history.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x4a; 32]));
        create_repository_fixture_with_version(&path, signer.clone(), 39).await;
        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &[
                "INSERT INTO effect_fences(scope,epoch,canonical_digest,created_at_ms) VALUES ('global',1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',10)",
                "INSERT INTO effect_grants(grant_id,effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,issued_at_ms,transitioned_at_ms) VALUES ('00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000011','global',1,'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',zeroblob(32),'issued',0,10,10)",
                "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms) VALUES ('00000000-0000-4000-8000-000000000101','00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000011',0,'issued','bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',10)",
                "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms) VALUES ('00000000-0000-4000-8000-000000000102','00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000011',1,'consuming','bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',11)",
                "UPDATE effect_grants SET status='consuming',state_epoch=1,transitioned_at_ms=11 WHERE grant_id='00000000-0000-4000-8000-000000000001'",
                "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms) VALUES ('00000000-0000-4000-8000-000000000103','00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000011',2,'abandoned','bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',12)",
                "UPDATE effect_grants SET status='abandoned',state_epoch=2,transitioned_at_ms=12 WHERE grant_id='00000000-0000-4000-8000-000000000001'",
                "INSERT INTO effect_fences(scope,epoch,canonical_digest,created_at_ms) VALUES ('global',2,'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',20)",
                "INSERT INTO effect_grants(grant_id,effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,issued_at_ms,transitioned_at_ms) VALUES ('00000000-0000-4000-8000-000000000002','00000000-0000-4000-8000-000000000012','global',2,'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',zeroblob(32),'issued',0,20,20)",
                "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms) VALUES ('00000000-0000-4000-8000-000000000201','00000000-0000-4000-8000-000000000002','00000000-0000-4000-8000-000000000012',0,'issued','dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',20)",
                "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms) VALUES ('00000000-0000-4000-8000-000000000202','00000000-0000-4000-8000-000000000002','00000000-0000-4000-8000-000000000012',1,'consuming','dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',21)",
                "UPDATE effect_grants SET status='consuming',state_epoch=1,transitioned_at_ms=21 WHERE grant_id='00000000-0000-4000-8000-000000000002'",
                "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms) VALUES ('00000000-0000-4000-8000-000000000203','00000000-0000-4000-8000-000000000002','00000000-0000-4000-8000-000000000012',2,'applied','dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',22)",
                "UPDATE effect_grants SET status='applied',state_epoch=2,transitioned_at_ms=22 WHERE grant_id='00000000-0000-4000-8000-000000000002'",
            ],
        )
        .await;
        let checkpoint_before = signer.checkpoint().unwrap().unwrap();
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.close().await;

        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );
        assert_eq!(
            signer.checkpoint().unwrap().unwrap().sequence,
            checkpoint_before.sequence + 1
        );
        assert_eq!(
            sqlite_integrity_commit_count(&path).await,
            commits_before + 1
        );
        let pool = existing_repository_pool(&path).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM effect_grants")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM effect_audit")
                .fetch_one(&pool)
                .await
                .unwrap(),
            5
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT abandon_reason FROM effect_grants WHERE grant_id='00000000-0000-4000-8000-000000000001'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            migrations::V39_LEGACY_ABANDON_REASON
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT abandon_reason FROM effect_audit WHERE audit_id='00000000-0000-4000-8000-000000000103'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            migrations::V39_LEGACY_ABANDON_REASON
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM effect_grants WHERE status!='abandoned' AND abandon_reason IS NOT NULL UNION ALL SELECT COUNT(*) FROM effect_audit WHERE event_kind!='abandoned' AND abandon_reason IS NOT NULL")
                .fetch_all(&pool)
                .await
                .unwrap(),
            vec![0, 0]
        );
        pool.close().await;

        let commits_after_upgrade = sqlite_integrity_commit_count(&path).await;
        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer).await;
        assert!(reopened.is_ok());
        assert_eq!(
            sqlite_integrity_commit_count(&path).await,
            commits_after_upgrade
        );
    }

    #[tokio::test]
    async fn v42_rejects_v40_and_v41_active_history_without_advancing_the_anchor() {
        for version in [40_i64, 41] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory
                .path()
                .join(format!("v{version}-active-effect-history.db"));
            let signer = Arc::new(InMemoryIntegritySigner::new_for_testing(
                [version as u8; 32],
            ));
            create_repository_fixture_with_version(&path, signer.clone(), version).await;
            execute_sql_batch_and_publish_integrity(
                &path,
                signer.clone(),
                &[
                    "INSERT INTO effect_fences(scope,epoch,canonical_digest,created_at_ms) VALUES ('global',1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',1)",
                    "INSERT INTO effect_grants(grant_id,effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,issued_at_ms,transitioned_at_ms,abandon_reason) VALUES ('00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000011','global',1,'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',zeroblob(32),'issued',0,1,1,NULL)",
                    "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms,abandon_reason) VALUES ('00000000-0000-4000-8000-000000000101','00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000011',0,'issued','bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',1,NULL)",
                ],
            )
            .await;
            let checkpoint_before = signer.checkpoint().unwrap();
            let commits_before = sqlite_integrity_commit_count(&path).await;

            let error =
                SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                    .await
                    .unwrap_err();

            assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
            assert_eq!(
                sqlite_schema_versions(&path).await,
                (1_i64..=version).collect::<Vec<_>>()
            );
            assert!(
                !sqlite_schema_object_exists(&path, "table", "effect_fenced_sink_bindings").await
            );
            assert_eq!(signer.checkpoint().unwrap(), checkpoint_before);
            assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        }
    }

    #[tokio::test]
    async fn v42_upgrades_v40_and_v41_terminal_history_without_fabricating_receipts() {
        for version in [40_i64, 41] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory
                .path()
                .join(format!("v{version}-terminal-effect-history.db"));
            let signer = Arc::new(InMemoryIntegritySigner::new_for_testing(
                [version as u8; 32],
            ));
            create_repository_fixture_with_version(&path, signer.clone(), version).await;
            execute_sql_batch_and_publish_integrity(
                &path,
                signer.clone(),
                &[
                    "INSERT INTO effect_fences(scope,epoch,canonical_digest,created_at_ms) VALUES ('global',1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',1)",
                    "INSERT INTO effect_grants(grant_id,effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,issued_at_ms,transitioned_at_ms,abandon_reason) VALUES ('00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000011','global',1,'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',zeroblob(32),'issued',0,1,1,NULL)",
                    "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms,abandon_reason) VALUES ('00000000-0000-4000-8000-000000000101','00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000011',0,'issued','bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',1,NULL)",
                    "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms,abandon_reason) VALUES ('00000000-0000-4000-8000-000000000102','00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000011',1,'consuming','bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',2,NULL)",
                    "UPDATE effect_grants SET status='consuming',state_epoch=1,transitioned_at_ms=2 WHERE grant_id='00000000-0000-4000-8000-000000000001'",
                    "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms,abandon_reason) VALUES ('00000000-0000-4000-8000-000000000103','00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000011',2,'abandoned','bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',3,'legacy pre-v42 terminal grant')",
                    "UPDATE effect_grants SET status='abandoned',state_epoch=2,transitioned_at_ms=3,abandon_reason='legacy pre-v42 terminal grant' WHERE grant_id='00000000-0000-4000-8000-000000000001'",
                ],
            )
            .await;

            let repository =
                SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                    .await
                    .unwrap();
            repository.close().await;
            assert_eq!(
                sqlite_schema_versions(&path).await,
                (1_i64..=migrations::CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
            );
            assert_eq!(
                sqlite_table_row_count(&path, "effect_fenced_sink_bindings").await,
                0
            );
            assert_eq!(
                sqlite_table_row_count(&path, "effect_fenced_receipts").await,
                0
            );
            assert_eq!(
                sqlite_table_row_count(&path, "effect_fenced_legacy_terminal_grants").await,
                1
            );
        }
    }

    #[tokio::test]
    async fn v43_upgrades_v42_terminal_receipts_and_rejects_authority_binding_tampering() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v42-post-release-fenced-receipt.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x43; 32]));
        create_repository_fixture_with_version(&path, signer.clone(), 42).await;

        let grant_id = "00000000-0000-4000-8000-000000000042";
        let effect_id = "00000000-0000-4000-8000-000000000052";
        let receipt_id = "00000000-0000-4000-8000-000000000062";
        let target_digest = "b".repeat(64);
        let canonical_digest = "a".repeat(64);
        let repository_path_binding = "c".repeat(64);
        let command_binding = v42_fenced_command_binding(
            "v42-repository",
            &repository_path_binding,
            1,
            "v42.test.sink",
            "1",
            "v42-authority",
            grant_id,
            effect_id,
            1,
            &target_digest,
            &canonical_digest,
        );
        let receipt_binding = v42_fenced_receipt_binding(
            receipt_id,
            "v42.test.sink",
            "1",
            "v42-authority",
            grant_id,
            effect_id,
            1,
            &target_digest,
            &command_binding,
        );
        let statements = vec![
            format!(
                "INSERT INTO effect_fences(scope,epoch,canonical_digest,created_at_ms) VALUES ('global',1,'{canonical_digest}',1)"
            ),
            format!(
                "INSERT INTO effect_grants(grant_id,effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,issued_at_ms,transitioned_at_ms,abandon_reason) VALUES ('{grant_id}','{effect_id}','global',1,'{target_digest}','{canonical_digest}',zeroblob(32),'issued',0,1,1,NULL)"
            ),
            format!(
                "INSERT INTO effect_fenced_sink_bindings(grant_id,effect_id,sink_identity,sink_version,sink_authority,command_binding,repository_instance_id,repository_path_binding,repository_key_epoch,fence_epoch,target_digest,canonical_digest) VALUES ('{grant_id}','{effect_id}','v42.test.sink','1','v42-authority','{command_binding}','v42-repository','{repository_path_binding}',1,1,'{target_digest}','{canonical_digest}')"
            ),
            format!(
                "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms,abandon_reason) VALUES ('00000000-0000-4000-8000-000000000071','{grant_id}','{effect_id}',0,'issued','{target_digest}','{canonical_digest}',1,NULL)"
            ),
            format!(
                "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms,abandon_reason) VALUES ('00000000-0000-4000-8000-000000000072','{grant_id}','{effect_id}',1,'consuming','{target_digest}','{canonical_digest}',2,NULL)"
            ),
            format!(
                "UPDATE effect_grants SET status='consuming',state_epoch=1,transitioned_at_ms=2 WHERE grant_id='{grant_id}'"
            ),
            format!(
                "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms,abandon_reason) VALUES ('00000000-0000-4000-8000-000000000073','{grant_id}','{effect_id}',2,'applied','{target_digest}','{canonical_digest}',3,NULL)"
            ),
            format!(
                "UPDATE effect_grants SET status='applied',state_epoch=2,transitioned_at_ms=3 WHERE grant_id='{grant_id}'"
            ),
            format!(
                "INSERT INTO effect_fenced_receipts(receipt_id,grant_id,effect_id,sink_identity,sink_version,sink_authority,command_binding,receipt_binding,outcome,fence_epoch,target_digest) VALUES ('{receipt_id}','{grant_id}','{effect_id}','v42.test.sink','1','v42-authority','{command_binding}','{receipt_binding}','applied',1,'{target_digest}')"
            ),
        ];
        let statement_refs = statements.iter().map(String::as_str).collect::<Vec<_>>();
        execute_sql_batch_and_publish_integrity(&path, signer.clone(), &statement_refs).await;

        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.verify_integrity().await.unwrap();
        repository.close().await;

        let pool = existing_repository_pool(&path).await;
        assert_eq!(
            sqlx::query_as::<_, (i64, i64, String)>(
                "SELECT authority_binding_format,authority_key_epoch,authority_key_fingerprint FROM effect_fenced_sink_bindings WHERE grant_id=?",
            )
            .bind(grant_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            (0, 0, String::new())
        );
        assert_eq!(
            sqlx::query_as::<_, (i64, i64, String)>(
                "SELECT authority_binding_format,authority_key_epoch,authority_key_fingerprint FROM effect_fenced_receipts WHERE grant_id=?",
            )
            .bind(grant_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            (0, 0, String::new())
        );
        pool.close().await;

        let authority_key_epoch = 7_i64;
        let authority_key_fingerprint = "d".repeat(64);
        let v43_command_binding =
            crate::mcp_platform::fenced_projection_sink::fenced_command_binding(
                "v42-repository",
                &repository_path_binding,
                1,
                "v42.test.sink",
                "1",
                "v42-authority",
                authority_key_epoch,
                &authority_key_fingerprint,
                grant_id,
                effect_id,
                1,
                &target_digest,
                &canonical_digest,
            );
        let v43_receipt_binding =
            crate::mcp_platform::fenced_projection_sink::fenced_receipt_binding(
                receipt_id,
                "v42.test.sink",
                "1",
                "v42-authority",
                u64::try_from(authority_key_epoch).unwrap(),
                &authority_key_fingerprint,
                grant_id,
                effect_id,
                1,
                &target_digest,
                &v43_command_binding,
            );
        let establish_v43_history = [
            "DROP TRIGGER effect_fenced_sink_bindings_are_immutable_update".to_string(),
            "DROP TRIGGER effect_fenced_receipts_are_immutable_update".to_string(),
            format!(
                "UPDATE effect_fenced_sink_bindings SET authority_binding_format=1,authority_key_epoch={authority_key_epoch},authority_key_fingerprint='{authority_key_fingerprint}',command_binding='{v43_command_binding}' WHERE grant_id='{grant_id}'"
            ),
            format!(
                "UPDATE effect_fenced_receipts SET authority_binding_format=1,authority_key_epoch={authority_key_epoch},authority_key_fingerprint='{authority_key_fingerprint}',command_binding='{v43_command_binding}',receipt_binding='{v43_receipt_binding}' WHERE grant_id='{grant_id}'"
            ),
            "CREATE TRIGGER effect_fenced_sink_bindings_are_immutable_update BEFORE UPDATE ON effect_fenced_sink_bindings BEGIN SELECT RAISE(ABORT,'fenced sink binding is immutable'); END".to_string(),
            "CREATE TRIGGER effect_fenced_receipts_are_immutable_update BEFORE UPDATE ON effect_fenced_receipts BEGIN SELECT RAISE(ABORT,'fenced receipt is immutable'); END".to_string(),
        ];
        let establish_v43_history_refs = establish_v43_history
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        execute_sql_batch_and_publish_integrity(&path, signer.clone(), &establish_v43_history_refs)
            .await;
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.verify_integrity().await.unwrap();
        repository.close().await;

        for (case, tamper_statement, restore_statement) in [
            (
                "sink binding format",
                format!(
                    "UPDATE effect_fenced_sink_bindings SET authority_binding_format=0 WHERE grant_id='{grant_id}'"
                ),
                format!(
                    "UPDATE effect_fenced_sink_bindings SET authority_binding_format=1 WHERE grant_id='{grant_id}'"
                ),
            ),
            (
                "receipt binding format",
                format!(
                    "UPDATE effect_fenced_receipts SET authority_binding_format=0 WHERE grant_id='{grant_id}'"
                ),
                format!(
                    "UPDATE effect_fenced_receipts SET authority_binding_format=1 WHERE grant_id='{grant_id}'"
                ),
            ),
            (
                "sink authority epoch",
                format!(
                    "UPDATE effect_fenced_sink_bindings SET authority_key_epoch=8 WHERE grant_id='{grant_id}'"
                ),
                format!(
                    "UPDATE effect_fenced_sink_bindings SET authority_key_epoch={authority_key_epoch} WHERE grant_id='{grant_id}'"
                ),
            ),
            (
                "receipt authority epoch",
                format!(
                    "UPDATE effect_fenced_receipts SET authority_key_epoch=8 WHERE grant_id='{grant_id}'"
                ),
                format!(
                    "UPDATE effect_fenced_receipts SET authority_key_epoch={authority_key_epoch} WHERE grant_id='{grant_id}'"
                ),
            ),
            (
                "sink authority fingerprint",
                format!(
                    "UPDATE effect_fenced_sink_bindings SET authority_key_fingerprint='e{tail}' WHERE grant_id='{grant_id}'",
                    tail = "d".repeat(63),
                ),
                format!(
                    "UPDATE effect_fenced_sink_bindings SET authority_key_fingerprint='{authority_key_fingerprint}' WHERE grant_id='{grant_id}'"
                ),
            ),
            (
                "receipt authority fingerprint",
                format!(
                    "UPDATE effect_fenced_receipts SET authority_key_fingerprint='e{tail}' WHERE grant_id='{grant_id}'",
                    tail = "d".repeat(63),
                ),
                format!(
                    "UPDATE effect_fenced_receipts SET authority_key_fingerprint='{authority_key_fingerprint}' WHERE grant_id='{grant_id}'"
                ),
            ),
            (
                "V43 command binding",
                format!(
                    "UPDATE effect_fenced_sink_bindings SET command_binding='{tampered}' WHERE grant_id='{grant_id}'",
                    tampered = "e".repeat(64),
                ),
                format!(
                    "UPDATE effect_fenced_sink_bindings SET command_binding='{v43_command_binding}' WHERE grant_id='{grant_id}'"
                ),
            ),
            (
                "receipt binding",
                format!(
                    "UPDATE effect_fenced_receipts SET receipt_binding='e{tail}' WHERE grant_id='{grant_id}'",
                    tail = "d".repeat(63),
                ),
                format!(
                    "UPDATE effect_fenced_receipts SET receipt_binding='{v43_receipt_binding}' WHERE grant_id='{grant_id}'"
                ),
            ),
        ] {
            let mut tamper_statements = vec![
                "DROP TRIGGER effect_fenced_sink_bindings_are_immutable_update".to_string(),
                "DROP TRIGGER effect_fenced_receipts_are_immutable_update".to_string(),
                tamper_statement,
            ];
            if case == "V43 command binding" {
                tamper_statements.push(format!(
                    "UPDATE effect_fenced_receipts SET command_binding='{command_binding}' WHERE grant_id='{grant_id}'",
                    command_binding = "e".repeat(64),
                ));
            }
            tamper_statements.extend([
                "CREATE TRIGGER effect_fenced_sink_bindings_are_immutable_update BEFORE UPDATE ON effect_fenced_sink_bindings BEGIN SELECT RAISE(ABORT,'fenced sink binding is immutable'); END".to_string(),
                "CREATE TRIGGER effect_fenced_receipts_are_immutable_update BEFORE UPDATE ON effect_fenced_receipts BEGIN SELECT RAISE(ABORT,'fenced receipt is immutable'); END".to_string(),
            ]);
            let tamper_statement_refs = tamper_statements
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            execute_sql_batch_and_publish_integrity(&path, signer.clone(), &tamper_statement_refs)
                .await;
            let error =
                SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                    .await
                    .unwrap_err();
            assert_eq!(
                error.code(),
                McpPlatformErrorCode::IntegrityError,
                "case {case}"
            );

            let mut restore_statements = vec![
                "DROP TRIGGER effect_fenced_sink_bindings_are_immutable_update".to_string(),
                "DROP TRIGGER effect_fenced_receipts_are_immutable_update".to_string(),
                restore_statement,
            ];
            if case == "V43 command binding" {
                restore_statements.push(format!(
                    "UPDATE effect_fenced_receipts SET command_binding='{v43_command_binding}' WHERE grant_id='{grant_id}'"
                ));
            }
            restore_statements.extend([
                "CREATE TRIGGER effect_fenced_sink_bindings_are_immutable_update BEFORE UPDATE ON effect_fenced_sink_bindings BEGIN SELECT RAISE(ABORT,'fenced sink binding is immutable'); END".to_string(),
                "CREATE TRIGGER effect_fenced_receipts_are_immutable_update BEFORE UPDATE ON effect_fenced_receipts BEGIN SELECT RAISE(ABORT,'fenced receipt is immutable'); END".to_string(),
            ]);
            let restore_statement_refs = restore_statements
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            execute_sql_batch_and_publish_integrity(&path, signer.clone(), &restore_statement_refs)
                .await;
            let repository =
                SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                    .await
                    .unwrap();
            repository.verify_integrity().await.unwrap();
            repository.close().await;
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn v42_anchored_fenced_receipt_semantic_tampering_fails_closed() {
        use crate::mcp_platform::repository::{
            FinishProjectionEffectGrant, IssueProjectionEffectGrant,
        };

        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("v42-fenced-receipt-semantic-tamper.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x42; 32]));
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let guard = repository.acquire_projection_effect_fence().await.unwrap();
        let effect_id = uuid::Uuid::new_v4().to_string();
        let grant = repository
            .issue_projection_effect_grant(
                &guard,
                IssueProjectionEffectGrant {
                    effect_id: &effect_id,
                    target_digest: &"a".repeat(64),
                    now_ms: 1,
                },
            )
            .await
            .unwrap();
        repository
            .consume_projection_effect_grant(&guard, &grant, 2)
            .await
            .unwrap();
        repository
            .finish_projection_effect_grant(
                &guard,
                FinishProjectionEffectGrant {
                    grant_id: grant.grant_id(),
                    now_ms: 3,
                },
            )
            .await
            .unwrap();
        drop(guard);
        repository.close().await;

        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &[
                "DROP TRIGGER effect_fenced_sink_bindings_are_immutable_update",
                "UPDATE effect_fenced_sink_bindings SET repository_key_epoch=2",
                "CREATE TRIGGER effect_fenced_sink_bindings_are_immutable_update BEFORE UPDATE ON effect_fenced_sink_bindings BEGIN SELECT RAISE(ABORT,'fenced sink binding is immutable'); END",
            ],
        )
        .await;

        let error = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[tokio::test]
    async fn v40_rejects_anchored_v39_incomplete_effect_history_without_migration_progress() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("v40-rejects-v39-incomplete-history.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x4b; 32]));
        create_repository_fixture_with_version(&path, signer.clone(), 39).await;
        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &[
                "INSERT INTO effect_fences(scope,epoch,canonical_digest,created_at_ms) VALUES ('global',1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',1)",
                "INSERT INTO effect_grants(grant_id,effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,issued_at_ms,transitioned_at_ms) VALUES ('00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000011','global',1,'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',zeroblob(32),'issued',0,1,1)",
                "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms) VALUES ('00000000-0000-4000-8000-000000000101','00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000011',0,'issued','bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',1)",
                "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms) VALUES ('00000000-0000-4000-8000-000000000102','00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000011',1,'applied','bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',2)",
            ],
        )
        .await;
        let checkpoint_before = signer.checkpoint().unwrap();
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let error =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=39).collect::<Vec<_>>()
        );
        assert!(!sqlite_table_has_column(&path, "effect_grants", "abandon_reason").await);
        assert_eq!(sqlite_table_row_count(&path, "effect_fences").await, 2);
        assert_eq!(sqlite_table_row_count(&path, "effect_grants").await, 1);
        assert_eq!(sqlite_table_row_count(&path, "effect_audit").await, 2);
        assert_eq!(signer.checkpoint().unwrap(), checkpoint_before);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn v41_effect_grants_have_one_active_lifecycle_and_recover_after_reopen() {
        use crate::mcp_platform::repository::{
            FinishProjectionEffectGrant, IssueProjectionEffectGrant, ProjectionEffectGrantStatus,
        };

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v41-single-lifecycle.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x41; 32]));
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let guard = repository.acquire_projection_effect_fence().await.unwrap();
        let first_effect = uuid::Uuid::new_v4().to_string();
        let concurrent_effect = uuid::Uuid::new_v4().to_string();
        let first_target_digest = "a".repeat(64);
        let concurrent_target_digest = "b".repeat(64);
        let (first, concurrent) = tokio::join!(
            repository.issue_projection_effect_grant(
                &guard,
                IssueProjectionEffectGrant {
                    effect_id: &first_effect,
                    target_digest: &first_target_digest,
                    now_ms: 10,
                },
            ),
            repository.issue_projection_effect_grant(
                &guard,
                IssueProjectionEffectGrant {
                    effect_id: &concurrent_effect,
                    target_digest: &concurrent_target_digest,
                    now_ms: 10,
                },
            ),
        );
        let grant = match (first, concurrent) {
            (Ok(grant), Err(_)) | (Err(_), Ok(grant)) => grant,
            _ => panic!("exactly one concurrent grant must issue"),
        };
        let checkpoint_after_issue = signer.checkpoint().unwrap();
        let rejected_effect = uuid::Uuid::new_v4().to_string();
        assert!(repository
            .issue_projection_effect_grant(
                &guard,
                IssueProjectionEffectGrant {
                    effect_id: &rejected_effect,
                    target_digest: &"c".repeat(64),
                    now_ms: 11,
                },
            )
            .await
            .is_err());
        assert!(repository
            .issue_projection_effect_grant(
                &guard,
                IssueProjectionEffectGrant {
                    effect_id: &grant.effect_id,
                    target_digest: &"a".repeat(64),
                    now_ms: 11,
                },
            )
            .await
            .is_err());
        assert_eq!(signer.checkpoint().unwrap(), checkpoint_after_issue);

        repository
            .consume_projection_effect_grant(&guard, &grant, 12)
            .await
            .unwrap();
        repository
            .mark_projection_effect_grant_unknown(
                &guard,
                FinishProjectionEffectGrant {
                    grant_id: grant.grant_id(),
                    now_ms: 13,
                },
            )
            .await
            .unwrap();
        assert!(repository
            .finish_projection_effect_grant(
                &guard,
                FinishProjectionEffectGrant {
                    grant_id: grant.grant_id(),
                    now_ms: 14,
                },
            )
            .await
            .is_err());
        let next_effect = uuid::Uuid::new_v4().to_string();
        let next_grant = repository
            .issue_projection_effect_grant(
                &guard,
                IssueProjectionEffectGrant {
                    effect_id: &next_effect,
                    target_digest: &"d".repeat(64),
                    now_ms: 15,
                },
            )
            .await
            .unwrap();
        assert_eq!(next_grant.fence_epoch, grant.fence_epoch + 1);
        drop(guard);
        repository.close().await;

        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let recovered_guard = reopened.acquire_projection_effect_fence().await.unwrap();
        reopened
            .consume_projection_effect_grant(&recovered_guard, &next_grant, 16)
            .await
            .unwrap();
        reopened
            .finish_projection_effect_grant(
                &recovered_guard,
                FinishProjectionEffectGrant {
                    grant_id: next_grant.grant_id(),
                    now_ms: 17,
                },
            )
            .await
            .unwrap();
        reopened.verify_integrity().await.unwrap();
        reopened.close().await;

        let pool = existing_repository_pool(&path).await;
        let statuses = sqlx::query_scalar::<_, String>(
            "SELECT status FROM effect_grants ORDER BY fence_epoch",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            statuses,
            vec![
                ProjectionEffectGrantStatus::Unknown.as_str().to_string(),
                ProjectionEffectGrantStatus::Applied.as_str().to_string(),
            ]
        );
        pool.close().await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn v41_anchor_advance_rejects_stale_guard_then_preserves_grant_recovery() {
        use crate::mcp_platform::repository::{
            FinishProjectionEffectGrant, IssueProjectionEffectGrant,
        };

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v41-stale-anchor.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x42; 32]));
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let guard = repository.acquire_projection_effect_fence().await.unwrap();
        let effect_id = uuid::Uuid::new_v4().to_string();
        let grant = repository
            .issue_projection_effect_grant(
                &guard,
                IssueProjectionEffectGrant {
                    effect_id: &effect_id,
                    target_digest: &"e".repeat(64),
                    now_ms: 10,
                },
            )
            .await
            .unwrap();
        let audit_id = uuid::Uuid::new_v4().to_string();
        let consuming_audit = format!(
            "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms,abandon_reason) VALUES ('{audit_id}','{}','{}',1,'consuming','{}','{}',11,NULL)",
            grant.grant_id(),
            grant.effect_id,
            "e".repeat(64),
            grant.canonical_digest,
        );
        let consuming_grant = format!(
            "UPDATE effect_grants SET status='consuming',state_epoch=1,transitioned_at_ms=11 WHERE grant_id='{}'",
            grant.grant_id(),
        );
        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &[&consuming_audit, &consuming_grant],
        )
        .await;
        assert!(repository
            .finish_projection_effect_grant(
                &guard,
                FinishProjectionEffectGrant {
                    grant_id: grant.grant_id(),
                    now_ms: 12,
                },
            )
            .await
            .is_err());
        drop(guard);
        let recovered_guard = repository.acquire_projection_effect_fence().await.unwrap();
        repository
            .finish_projection_effect_grant(
                &recovered_guard,
                FinishProjectionEffectGrant {
                    grant_id: grant.grant_id(),
                    now_ms: 12,
                },
            )
            .await
            .unwrap();
        repository.verify_integrity().await.unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn v39_effect_grant_is_one_shot_and_never_persists_the_nonce() {
        use crate::mcp_platform::repository::{
            FinishProjectionEffectGrant, IssueProjectionEffectGrant, ProjectionEffectGrantStatus,
        };

        let (directory, repository) =
            open_temp_repository_with_test_signer("v39-one-shot.db", [0x3a; 32]).await;
        let effect_id = uuid::Uuid::new_v4().to_string();
        let guard = repository.acquire_projection_effect_fence().await.unwrap();
        let grant = repository
            .issue_projection_effect_grant(
                &guard,
                IssueProjectionEffectGrant {
                    effect_id: &effect_id,
                    target_digest: &"a".repeat(64),
                    now_ms: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(grant.effect_id, effect_id);
        assert_eq!(grant.fence_epoch, 1);
        assert_eq!(grant.canonical_digest.len(), 64);
        repository
            .consume_projection_effect_grant(&guard, &grant, 11)
            .await
            .unwrap();
        repository
            .finish_projection_effect_grant(
                &guard,
                FinishProjectionEffectGrant {
                    grant_id: &grant.grant_id,
                    now_ms: 12,
                },
            )
            .await
            .unwrap();
        let repeated_finish = repository
            .finish_projection_effect_grant(
                &guard,
                FinishProjectionEffectGrant {
                    grant_id: &grant.grant_id,
                    now_ms: 13,
                },
            )
            .await;
        assert!(repeated_finish.is_err());
        let duplicate_effect = repository
            .issue_projection_effect_grant(
                &guard,
                IssueProjectionEffectGrant {
                    effect_id: &effect_id,
                    target_digest: &"a".repeat(64),
                    now_ms: 14,
                },
            )
            .await;
        assert!(duplicate_effect.is_err());
        repository.close().await;

        let path = directory.path().join("v39-one-shot.db");
        let pool = existing_repository_pool(&path).await;
        let status: String =
            sqlx::query_scalar("SELECT status FROM effect_grants WHERE grant_id=?")
                .bind(&grant.grant_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, ProjectionEffectGrantStatus::Applied.as_str());
        let stored_nonce_hash: Vec<u8> =
            sqlx::query_scalar("SELECT nonce_hash FROM effect_grants WHERE grant_id=?")
                .bind(&grant.grant_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_ne!(stored_nonce_hash.as_slice(), grant.nonce.as_slice());
        let maximum_epoch: i64 =
            sqlx::query_scalar("SELECT MAX(epoch) FROM effect_fences WHERE scope='global'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(maximum_epoch, 1);
        let audit_text: String = sqlx::query_scalar(
            "SELECT group_concat(audit_id || grant_id || effect_id || event_kind || target_digest || canonical_digest, '') FROM effect_audit",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!audit_text.contains(&crate::utils::bytes_to_hex(grant.nonce)));
        let audit_columns = sqlx::query("PRAGMA table_info('effect_audit')")
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.try_get::<String, _>("name").unwrap())
            .collect::<Vec<_>>();
        assert!(audit_columns.iter().all(|column| {
            !["config", "token", "url", "path", "nonce"]
                .iter()
                .any(|secret| column.contains(secret))
        }));
        pool.close().await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn v39_effect_grant_direct_mutations_and_schema_tamper_fail_closed() {
        use crate::mcp_platform::repository::IssueProjectionEffectGrant;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v39-tamper.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x3b; 32]));
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let effect_id = uuid::Uuid::new_v4().to_string();
        let guard = repository.acquire_projection_effect_fence().await.unwrap();
        let grant = repository
            .issue_projection_effect_grant(
                &guard,
                IssueProjectionEffectGrant {
                    effect_id: &effect_id,
                    target_digest: &"b".repeat(64),
                    now_ms: 20,
                },
            )
            .await
            .unwrap();
        repository.close().await;

        let pool = existing_repository_pool(&path).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        sqlx::query("PRAGMA recursive_triggers=ON")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA recursive_triggers")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        for statement in [
            "UPDATE effect_grants SET target_digest='cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc'",
            "DELETE FROM effect_grants",
            "UPDATE effect_fences SET epoch=99",
            "INSERT INTO effect_grants(grant_id,effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,issued_at_ms,transitioned_at_ms) VALUES ('not-a-uuid','not-a-uuid','global',1,'bad','bad',x'00','issued',0,0,0)",
            "INSERT INTO effect_fences(scope,epoch,canonical_digest,created_at_ms) VALUES ('global',2,x'00',0)",
            "INSERT INTO effect_fences(scope,epoch,canonical_digest,created_at_ms) VALUES ('global',2,'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',1.5)",
        ] {
            assert!(sqlx::query(statement).execute(&pool).await.is_err());
        }
        assert!(sqlx::query("INSERT OR REPLACE INTO effect_grants(grant_id,effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,issued_at_ms,transitioned_at_ms) SELECT grant_id,effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,issued_at_ms,transitioned_at_ms FROM effect_grants WHERE grant_id=?")
            .bind(&grant.grant_id)
            .execute(&pool)
            .await
            .is_err());
        assert!(sqlx::query("INSERT INTO effect_grants(grant_id,effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,issued_at_ms,transitioned_at_ms) SELECT grant_id,effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,issued_at_ms,transitioned_at_ms FROM effect_grants WHERE grant_id=? ON CONFLICT(grant_id) DO UPDATE SET status='consuming'")
            .bind(&grant.grant_id)
            .execute(&pool)
            .await
            .is_err());
        assert!(sqlx::query("DROP TRIGGER effect_grants_are_not_deleted")
            .execute(&pool)
            .await
            .is_ok());
        pool.close().await;
        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer).await;
        let error = reopened.unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert!(!grant.grant_id.is_empty());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn v40_effect_lifecycle_is_anchored_once_per_authorized_transition() {
        use crate::mcp_platform::repository::{
            AbandonProjectionEffectGrant, FinishProjectionEffectGrant, IssueProjectionEffectGrant,
        };

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v40-effect-lifecycle.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x40; 32]));
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let initial = signer.checkpoint().unwrap().unwrap();
        let effect_id = uuid::Uuid::new_v4().to_string();
        let guard = repository.acquire_projection_effect_fence().await.unwrap();
        let grant = repository
            .issue_projection_effect_grant(
                &guard,
                IssueProjectionEffectGrant {
                    effect_id: &effect_id,
                    target_digest: &"a".repeat(64),
                    now_ms: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            signer.checkpoint().unwrap().unwrap().sequence,
            initial.sequence + 1
        );
        repository
            .consume_projection_effect_grant(&guard, &grant, 11)
            .await
            .unwrap();
        assert_eq!(
            signer.checkpoint().unwrap().unwrap().sequence,
            initial.sequence + 2
        );
        repository
            .finish_projection_effect_grant(
                &guard,
                FinishProjectionEffectGrant {
                    grant_id: &grant.grant_id,
                    now_ms: 12,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            signer.checkpoint().unwrap().unwrap().sequence,
            initial.sequence + 3
        );

        let abandoned_effect_id = uuid::Uuid::new_v4().to_string();
        let abandoned = repository
            .issue_projection_effect_grant(
                &guard,
                IssueProjectionEffectGrant {
                    effect_id: &abandoned_effect_id,
                    target_digest: &"b".repeat(64),
                    now_ms: 13,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            signer.checkpoint().unwrap().unwrap().sequence,
            initial.sequence + 4
        );
        repository
            .abandon_projection_effect_grant(
                &guard,
                AbandonProjectionEffectGrant {
                    grant_id: &abandoned.grant_id,
                    reason: "sink-unavailable",
                    now_ms: 14,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            signer.checkpoint().unwrap().unwrap().sequence,
            initial.sequence + 5
        );
        assert!(repository
            .abandon_projection_effect_grant(
                &guard,
                AbandonProjectionEffectGrant {
                    grant_id: &abandoned.grant_id,
                    reason: "replay",
                    now_ms: 15,
                }
            )
            .await
            .is_err());
        repository.close().await;

        let pool = existing_repository_pool(&path).await;
        let reason: String =
            sqlx::query_scalar("SELECT abandon_reason FROM effect_grants WHERE grant_id=?")
                .bind(&abandoned.grant_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(reason, "sink-unavailable");
        let nonce_text: String = sqlx::query_scalar("SELECT group_concat(COALESCE(abandon_reason,'') || target_digest || canonical_digest, '') FROM effect_audit")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!nonce_text.contains(&crate::utils::bytes_to_hex(abandoned.nonce)));
        pool.close().await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn v40_valid_shaped_direct_effect_forgery_fails_against_unchanged_anchor() {
        use crate::mcp_platform::repository::IssueProjectionEffectGrant;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v40-anchored-forgery.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x41; 32]));
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let effect_id = uuid::Uuid::new_v4().to_string();
        let guard = repository.acquire_projection_effect_fence().await.unwrap();
        let grant = repository
            .issue_projection_effect_grant(
                &guard,
                IssueProjectionEffectGrant {
                    effect_id: &effect_id,
                    target_digest: &"c".repeat(64),
                    now_ms: 20,
                },
            )
            .await
            .unwrap();
        let anchor_before = signer.checkpoint().unwrap();
        let commits_before = sqlite_integrity_commit_count(&path).await;
        let pool = existing_repository_pool(&path).await;
        let audit_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        assert!(sqlx::query("INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms,abandon_reason) VALUES (?,?,?,1,'consuming',?,?,21,NULL)")
            .bind(audit_id)
            .bind(&grant.grant_id)
            .bind(&grant.effect_id)
            .bind(&"c".repeat(64))
            .bind(&grant.canonical_digest)
            .execute(&pool)
            .await
            .is_ok());
        assert!(sqlx::query("UPDATE effect_grants SET status='consuming',state_epoch=1,transitioned_at_ms=21,abandon_reason=NULL WHERE grant_id=?")
            .bind(&grant.grant_id)
            .execute(&pool)
            .await
            .is_ok());
        pool.close().await;

        assert!(repository.verify_integrity().await.is_err());
        assert!(repository.recovery_eligibility().await.is_err());
        assert_eq!(signer.checkpoint().unwrap(), anchor_before);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        repository.close().await;
        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await;
        assert_eq!(
            reopened.unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError
        );
        assert_eq!(signer.checkpoint().unwrap(), anchor_before);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
    }

    #[tokio::test]
    async fn v40_rejects_anchored_v39_rows_with_noncanonical_uuid_grammar() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v40-rejects-v39-uuid.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x42; 32]));
        create_repository_fixture_with_version(&path, signer.clone(), 39).await;
        let malformed = "------------------------------------";
        execute_sql_batch_and_publish_integrity(
            &path,
            signer.clone(),
            &[
                "INSERT INTO effect_fences(scope,epoch,canonical_digest,created_at_ms) VALUES ('global',1,'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',1)",
                "INSERT INTO effect_grants(grant_id,effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,issued_at_ms,transitioned_at_ms) VALUES ('------------------------------------','------------------------------------','global',1,'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc','dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',zeroblob(32),'issued',0,1,1)",
                "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms) VALUES ('------------------------------------','------------------------------------','------------------------------------',0,'issued','cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc','dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',1)",
            ],
        )
        .await;
        let checkpoint_before = signer.checkpoint().unwrap();
        let commits_before = sqlite_integrity_commit_count(&path).await;

        let error =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            sqlite_schema_versions(&path).await,
            (1..=39).collect::<Vec<_>>()
        );
        assert_eq!(signer.checkpoint().unwrap(), checkpoint_before);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        assert!(!malformed.is_empty());
    }

    #[tokio::test]
    async fn v40_effect_index_and_sqlite_master_tampering_fail_closed_without_anchor_advance() {
        for (name, statements) in [
            ("index", vec!["DROP INDEX effect_grants_status"]),
            (
                "sqlite-master",
                vec![
                    "PRAGMA writable_schema=ON",
                    "UPDATE sqlite_master SET sql=sql || ' ' WHERE type='index' AND name='effect_audit_effect_id'",
                    "PRAGMA writable_schema=OFF",
                ],
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(format!("v40-{name}-tamper.db"));
            let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x43; 32]));
            let repository =
                SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                    .await
                    .unwrap();
            repository.close().await;
            let anchor_before = signer.checkpoint().unwrap();
            let commits_before = sqlite_integrity_commit_count(&path).await;
            let pool = existing_repository_pool(&path).await;
            for statement in statements {
                sqlx::query(statement).execute(&pool).await.unwrap();
            }
            pool.close().await;

            assert!(SqliteMcpPlatformRepository::open_path_with_integrity_signer(
                &path,
                signer.clone(),
            )
            .await
            .is_err());
            assert_eq!(signer.checkpoint().unwrap(), anchor_before);
            assert_eq!(sqlite_integrity_commit_count(&path).await, commits_before);
        }
    }

    #[tokio::test]
    async fn https_manifest_provision_expiry_rejects_consume_and_allows_only_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("https-manifest-provision-v45.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x45; 32]));
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let bytes = b"https manifest".to_vec();
        let token_hash = "a".repeat(64);
        let raw_digest = crate::utils::bytes_to_hex(Sha256::digest(&bytes));
        for dns_evidence_digest in [None, Some("D".repeat(64))] {
            let error = repository
                .save_https_manifest_provision(
                    HttpsManifestProvisionRecord {
                        provision_id: format!("invalid-dns-{}", dns_evidence_digest.is_some()),
                        actor: "actor-v45".into(),
                        transport_session_binding: "session-v45".into(),
                        requested_url: None,
                        requested_url_id: "requested-v45".into(),
                        final_url_id: "final-v45".into(),
                        raw_digest: raw_digest.clone(),
                        parsed_digest: "b".repeat(64),
                        redirect_chain_digest: "c".repeat(64),
                        dns_evidence_digest,
                        frozen_bytes: bytes.clone(),
                        expires_at_ms: 15,
                        status: "saved".into(),
                        created_at_ms: 1,
                    },
                    &token_hash,
                )
                .await
                .expect_err("invalid DNS evidence must be rejected");
            assert_eq!(error.code(), McpPlatformErrorCode::ManifestConflict);
        }
        repository
            .save_https_manifest_provision(
                HttpsManifestProvisionRecord {
                    provision_id: "provision-v45".into(),
                    actor: "actor-v45".into(),
                    transport_session_binding: "session-v45".into(),
                    requested_url: Some("https://example.com/manifest".into()),
                    requested_url_id: crate::utils::bytes_to_hex(Sha256::digest(
                        b"https://example.com/manifest",
                    )),
                    final_url_id: "final-v45".into(),
                    raw_digest: raw_digest.clone(),
                    parsed_digest: "b".repeat(64),
                    redirect_chain_digest: "c".repeat(64),
                    dns_evidence_digest: Some("d".repeat(64)),
                    frozen_bytes: bytes.clone(),
                    expires_at_ms: 15,
                    status: "saved".into(),
                    created_at_ms: 1,
                },
                &token_hash,
            )
            .await
            .unwrap();

        let claimed = repository
            .claim_https_manifest_provision(
                "provision-v45",
                &token_hash,
                "actor-v45",
                "session-v45",
                10,
            )
            .await
            .unwrap();
        assert_eq!(claimed.status, "claimed");
        assert_eq!(claimed.dns_evidence_digest, Some("d".repeat(64)));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT claim_lease_until_ms FROM https_manifest_provisions WHERE provision_id='provision-v45'",
            )
            .fetch_one(&repository.pool)
            .await
            .unwrap(),
            10 + migrations::HTTPS_MANIFEST_CLAIM_LEASE_MS
        );
        let lease_column = sqlx::query("PRAGMA table_info('https_manifest_provisions')")
            .fetch_all(&repository.pool)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.get::<String, _>("name") == "claim_lease_until_ms")
            .expect("v45 must add claim_lease_until_ms");
        assert_eq!(lease_column.get::<String, _>("type"), "INTEGER");
        assert_eq!(lease_column.get::<i64, _>("notnull"), 0);
        assert!(!format!("{claimed:?}").contains(&token_hash));

        let second_token_hash = "d".repeat(64);
        repository
            .save_https_manifest_provision(
                HttpsManifestProvisionRecord {
                    provision_id: "provision-v45-lease-expired".into(),
                    actor: "actor-v45".into(),
                    transport_session_binding: "session-v45".into(),
                    requested_url: Some("https://example.com/manifest".into()),
                    requested_url_id: crate::utils::bytes_to_hex(Sha256::digest(
                        b"https://example.com/manifest",
                    )),
                    final_url_id: "final-v45".into(),
                    raw_digest,
                    parsed_digest: "e".repeat(64),
                    redirect_chain_digest: "f".repeat(64),
                    dns_evidence_digest: Some("1".repeat(64)),
                    frozen_bytes: bytes,
                    expires_at_ms: 100,
                    status: "saved".into(),
                    created_at_ms: 1,
                },
                &second_token_hash,
            )
            .await
            .unwrap();
        repository
            .claim_https_manifest_provision(
                "provision-v45-lease-expired",
                &second_token_hash,
                "actor-v45",
                "session-v45",
                10,
            )
            .await
            .unwrap();
        assert!(repository
            .consume_https_manifest_provision(
                "provision-v45-lease-expired",
                &second_token_hash,
                "actor-v45",
                "session-v45",
                10 + migrations::HTTPS_MANIFEST_CLAIM_LEASE_MS + 1
            )
            .await
            .is_err());
        assert_eq!(sqlx::query_scalar::<_, String>("SELECT status FROM https_manifest_provisions WHERE provision_id='provision-v45-lease-expired'").fetch_one(&repository.pool).await.unwrap(), "claimed");
        assert!(repository
            .consume_https_manifest_provision(
                "provision-v45",
                &token_hash,
                "actor-v45",
                "session-v45",
                20,
            )
            .await
            .is_err());
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM https_manifest_provisions WHERE provision_id='provision-v45'"
            )
            .fetch_one(&repository.pool)
            .await
            .unwrap(),
            "claimed"
        );
        assert!(repository
            .consume_https_manifest_provision(
                "provision-v45",
                &token_hash,
                "wrong-actor",
                "session-v45",
                20,
            )
            .await
            .is_err());
        assert!(repository
            .reject_https_manifest_provision(
                "provision-v45",
                &token_hash,
                "wrong-actor",
                "session-v45",
                20,
            )
            .await
            .is_err());
        assert!(repository
            .reject_https_manifest_provision(
                "provision-v45",
                &token_hash,
                "actor-v45",
                "wrong-session",
                20
            )
            .await
            .is_err());
        assert!(repository
            .reject_https_manifest_provision(
                "provision-v45",
                &"b".repeat(64),
                "actor-v45",
                "session-v45",
                20
            )
            .await
            .is_err());
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM https_manifest_provisions WHERE provision_id='provision-v45'"
            )
            .fetch_one(&repository.pool)
            .await
            .unwrap(),
            "claimed"
        );
        repository
            .reject_https_manifest_provision(
                "provision-v45",
                &token_hash,
                "actor-v45",
                "session-v45",
                20,
            )
            .await
            .unwrap();
        assert!(repository
            .reject_https_manifest_provision(
                "provision-v45",
                &token_hash,
                "actor-v45",
                "session-v45",
                21,
            )
            .await
            .is_err());
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM https_manifest_provisions WHERE provision_id='provision-v45'",
            )
            .fetch_one(&repository.pool)
            .await
            .unwrap(),
            "rejected"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pragma_index_list('https_manifest_provisions') WHERE name='https_manifest_provisions_claim_lease'",
            )
            .fetch_one(&repository.pool)
            .await
            .unwrap(),
            1
        );
        let reopen_token_hash = "e".repeat(64);
        repository
            .save_https_manifest_provision(
                HttpsManifestProvisionRecord {
                    provision_id: "provision-v45-reopen".into(),
                    actor: "actor-v45-reopen".into(),
                    transport_session_binding: "session-v45-reopen".into(),
                    requested_url: Some("https://example.com/manifest".into()),
                    requested_url_id: crate::utils::bytes_to_hex(Sha256::digest(
                        b"https://example.com/manifest",
                    )),
                    final_url_id: "final-v45-reopen".into(),
                    raw_digest: crate::utils::bytes_to_hex(Sha256::digest(b"reopen manifest")),
                    parsed_digest: "2".repeat(64),
                    redirect_chain_digest: "3".repeat(64),
                    dns_evidence_digest: Some("4".repeat(64)),
                    frozen_bytes: b"reopen manifest".to_vec(),
                    expires_at_ms: 1_000,
                    status: "saved".into(),
                    created_at_ms: 1,
                },
                &reopen_token_hash,
            )
            .await
            .unwrap();
        repository.close().await;
        let reopened =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let reopened_claim = reopened
            .claim_https_manifest_provision(
                "provision-v45-reopen",
                &reopen_token_hash,
                "actor-v45-reopen",
                "session-v45-reopen",
                10,
            )
            .await
            .unwrap();
        assert_eq!(reopened_claim.status, "claimed");
        reopened.close().await;
    }

    #[tokio::test]
    async fn v45_null_dns_records_upgrade_and_consume_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v45-null-dns.db");
        create_schema_fixture_with_version(&path, 45).await;
        let pool = existing_repository_pool(&path).await;
        sqlx::query("INSERT INTO https_manifest_provisions (provision_id,actor,transport_session_binding,requested_url_id,final_url_id,raw_digest,parsed_digest,redirect_chain_digest,frozen_bytes,token_hash,expires_at_ms,status,created_at_ms,claimed_at_ms,claim_lease_until_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind("legacy-null-dns")
            .bind("legacy-actor")
            .bind("legacy-binding")
            .bind("legacy-request")
            .bind("legacy-final")
            .bind("a".repeat(64))
            .bind("b".repeat(64))
            .bind("c".repeat(64))
            .bind(Vec::<u8>::new())
            .bind("d".repeat(64))
            .bind(100_i64)
            .bind("claimed")
            .bind(1_i64)
            .bind(2_i64)
            .bind(90_i64)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x46; 32]));
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        assert!(repository
            .consume_https_manifest_provision(
                "legacy-null-dns",
                &"d".repeat(64),
                "legacy-actor",
                "legacy-binding",
                10,
            )
            .await
            .is_err());
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM https_manifest_provisions WHERE provision_id='legacy-null-dns'"
            )
            .fetch_one(&repository.pool)
            .await
            .unwrap(),
            "claimed"
        );
        repository
            .reject_https_manifest_provision(
                "legacy-null-dns",
                &"d".repeat(64),
                "legacy-actor",
                "legacy-binding",
                10,
            )
            .await
            .unwrap();
        assert!(repository
            .consume_https_manifest_provision(
                "legacy-null-dns",
                &"d".repeat(64),
                "legacy-actor",
                "legacy-binding",
                11,
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn v46_dns_evidence_claim_and_consume_normal_path_succeeds() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v46-dns-normal.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x47; 32]));
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        let token_hash = "a".repeat(64);
        let requested_url = "https://example.com/manifest".to_string();
        let requested_url_id = crate::utils::bytes_to_hex(Sha256::digest(requested_url.as_bytes()));
        repository
            .save_https_manifest_provision(
                HttpsManifestProvisionRecord {
                    provision_id: "normal-dns".into(),
                    actor: "actor".into(),
                    transport_session_binding: "session".into(),
                    requested_url: Some(requested_url),
                    requested_url_id,
                    final_url_id: "final".into(),
                    raw_digest: crate::utils::bytes_to_hex(Sha256::digest(b"body")),
                    parsed_digest: "b".repeat(64),
                    redirect_chain_digest: "c".repeat(64),
                    dns_evidence_digest: Some("d".repeat(64)),
                    frozen_bytes: b"body".to_vec(),
                    expires_at_ms: 100,
                    status: "saved".into(),
                    created_at_ms: 1,
                },
                &token_hash,
            )
            .await
            .unwrap();
        repository
            .claim_https_manifest_provision("normal-dns", &token_hash, "actor", "session", 10)
            .await
            .unwrap();
        repository
            .consume_https_manifest_provision("normal-dns", &token_hash, "actor", "session", 20)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM https_manifest_provisions WHERE provision_id='normal-dns'"
            )
            .fetch_one(&repository.pool)
            .await
            .unwrap(),
            "consumed"
        );
    }

    #[tokio::test]
    async fn v46_finalize_claimed_null_dns_is_integrity_error_and_unenumerable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v46-finalize-null-dns.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x51; 32],
            database_path_binding(&path).unwrap(),
        ));
        create_schema_fixture_with_version(&path, 45).await;
        let pool = existing_repository_pool(&path).await;
        sqlx::query("INSERT INTO https_manifest_provisions (provision_id,actor,transport_session_binding,requested_url_id,final_url_id,raw_digest,parsed_digest,redirect_chain_digest,frozen_bytes,token_hash,expires_at_ms,status,created_at_ms,claimed_at_ms,claim_lease_until_ms,terminal_at_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind("null-dns-finalize").bind("actor").bind("session").bind(crate::utils::bytes_to_hex(Sha256::digest(b"https://example.com/manifest")))
            .bind("final").bind(crate::utils::bytes_to_hex(Sha256::digest(b"fixture"))).bind("b".repeat(64)).bind("c".repeat(64))
            .bind(b"fixture".to_vec()).bind("f".repeat(64)).bind(100_i64).bind("claimed").bind(1_i64)
            .bind(2_i64).bind(90_i64).bind(None::<i64>)
            .execute(&pool).await.unwrap();
        pool.close().await;
        initialize_integrity_fixture(&path, signer.clone()).await;
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let manifest = ManifestRecord {
            verified: parse_manifest(REMOTE.as_bytes()).unwrap(),
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            source_metadata: ManifestSourceMetadata {
                source_ref: crate::mcp_platform::manifest::SourceRef::HttpsManifestUrl {
                    manifest_url: "https://example.com/manifest".into(),
                },
                ..Default::default()
            },
            created_at_ms: 1,
        };
        let snapshot = || async {
            sqlx::query_as::<_, (Option<String>, String, String, String, String, String, Option<String>, Vec<u8>, String, i64, String, i64, Option<i64>, Option<i64>, Option<i64>)>("SELECT requested_url,status,actor,transport_session_binding,requested_url_id,final_url_id,dns_evidence_digest,frozen_bytes,token_hash,expires_at_ms,raw_digest,created_at_ms,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='null-dns-finalize'")
                .fetch_one(&repository.pool).await.unwrap()
        };
        let before = snapshot().await;
        let blobs = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM manifest_blobs")
            .fetch_one(&repository.pool)
            .await
            .unwrap();
        let checkpoint = signer.checkpoint();
        let commits = sqlite_integrity_commit_count(&path).await;
        let error = repository
            .finalize_https_manifest_provision(
                "null-dns-finalize",
                &"f".repeat(64),
                "actor",
                "session",
                10,
                &manifest,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        let after = snapshot().await;
        assert_eq!(after.0, before.0);
        assert_eq!(after.1, before.1);
        assert_eq!(after.2, before.2);
        assert_eq!(after.3, before.3);
        assert_eq!(after.4, before.4);
        assert_eq!(after.5, before.5);
        assert_eq!(after.6, before.6);
        assert_eq!(after.7, before.7);
        assert_eq!(after.8, before.8);
        assert_eq!(after.9, before.9);
        assert_eq!(after.10, before.10);
        assert_eq!(after.11, before.11);
        assert_eq!(after.12, before.12);
        assert_eq!(after.13, before.13);
        assert_eq!(after.14, before.14);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM manifest_blobs")
                .fetch_one(&repository.pool)
                .await
                .unwrap(),
            blobs
        );
        assert_eq!(signer.checkpoint(), checkpoint);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits);
        for (token, actor, session, now_ms) in [
            ("0".repeat(64), "actor", "session", 10),
            ("f".repeat(64), "wrong-actor", "session", 10),
            ("f".repeat(64), "actor", "wrong-session", 10),
            ("f".repeat(64), "actor", "session", 100),
        ] {
            let error = repository
                .finalize_https_manifest_provision(
                    "null-dns-finalize",
                    &token,
                    actor,
                    session,
                    now_ms,
                    &manifest,
                )
                .await
                .unwrap_err();
            assert_eq!(error.code(), McpPlatformErrorCode::NotFound);
            let after = snapshot().await;
            assert_eq!(after.0, before.0);
            assert_eq!(after.1, before.1);
            assert_eq!(after.2, before.2);
            assert_eq!(after.3, before.3);
            assert_eq!(after.4, before.4);
            assert_eq!(after.5, before.5);
            assert_eq!(after.6, before.6);
            assert_eq!(after.7, before.7);
            assert_eq!(after.8, before.8);
            assert_eq!(after.9, before.9);
            assert_eq!(after.10, before.10);
            assert_eq!(after.11, before.11);
            assert_eq!(after.12, before.12);
            assert_eq!(after.13, before.13);
            assert_eq!(after.14, before.14);
            assert_eq!(signer.checkpoint(), checkpoint);
            assert_eq!(sqlite_integrity_commit_count(&path).await, commits);
        }
        repository
            .reject_https_manifest_provision(
                "null-dns-finalize",
                &"f".repeat(64),
                "actor",
                "session",
                100,
            )
            .await
            .unwrap();
        assert_eq!(sqlx::query_scalar::<_, String>("SELECT status FROM https_manifest_provisions WHERE provision_id='null-dns-finalize'").fetch_one(&repository.pool).await.unwrap(), "rejected");
        assert_eq!(
            repository
                .reject_https_manifest_provision(
                    "null-dns-finalize",
                    &"f".repeat(64),
                    "actor",
                    "session",
                    101
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::NotFound
        );
    }

    #[tokio::test]
    async fn v46_null_dns_saved_and_claimed_rows_reject_without_mutation() {
        for (status, claimable) in [("saved", true), ("claimed", false)] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(format!("v46-null-{status}.db"));
            let signer = Arc::new(InMemoryIntegritySigner::new_for_testing_with_path_binding(
                [0x48; 32],
                database_path_binding(&path).unwrap(),
            ));
            create_schema_fixture_with_version(&path, 45).await;
            let pool = existing_repository_pool(&path).await;
            sqlx::query("INSERT INTO https_manifest_provisions (provision_id,actor,transport_session_binding,requested_url_id,final_url_id,raw_digest,parsed_digest,redirect_chain_digest,frozen_bytes,token_hash,expires_at_ms,status,created_at_ms,claimed_at_ms,claim_lease_until_ms,terminal_at_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
                .bind("null-dns") .bind("actor") .bind("session") .bind("request") .bind("final")
                .bind("a".repeat(64)) .bind("b".repeat(64)) .bind("c".repeat(64)) .bind(Vec::<u8>::new())
                .bind("d".repeat(64)) .bind(100_i64) .bind(status) .bind(1_i64)
                .bind((status == "claimed").then_some(2_i64))
                .bind((status == "claimed").then_some(90_i64))
                .bind(None::<i64>)
                .execute(&pool).await.unwrap();
            pool.close().await;
            initialize_integrity_fixture(&path, signer.clone()).await;
            let repository =
                SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                    .await
                    .unwrap();
            let before: (String, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as("SELECT status, claimed_at_ms, claim_lease_until_ms, terminal_at_ms FROM https_manifest_provisions WHERE provision_id='null-dns'").fetch_one(&repository.pool).await.unwrap();
            let checkpoint = signer.checkpoint();
            let commits = sqlite_integrity_commit_count(&path).await;
            let claim = repository
                .claim_https_manifest_provision("null-dns", &"d".repeat(64), "actor", "session", 10)
                .await;
            assert!(claim.is_err());
            let after: (String, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as("SELECT status, claimed_at_ms, claim_lease_until_ms, terminal_at_ms FROM https_manifest_provisions WHERE provision_id='null-dns'").fetch_one(&repository.pool).await.unwrap();
            assert_eq!(after, before);
            assert_eq!(signer.checkpoint(), checkpoint);
            assert_eq!(sqlite_integrity_commit_count(&path).await, commits);
            if !claimable {
                assert!(repository
                    .consume_https_manifest_provision(
                        "null-dns",
                        &"d".repeat(64),
                        "actor",
                        "session",
                        10
                    )
                    .await
                    .is_err());
                let after_consume: (String, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as("SELECT status, claimed_at_ms, claim_lease_until_ms, terminal_at_ms FROM https_manifest_provisions WHERE provision_id='null-dns'").fetch_one(&repository.pool).await.unwrap();
                assert_eq!(after_consume, before);
                assert_eq!(signer.checkpoint(), checkpoint);
                assert_eq!(sqlite_integrity_commit_count(&path).await, commits);
                repository
                    .reject_https_manifest_provision(
                        "null-dns",
                        &"d".repeat(64),
                        "actor",
                        "session",
                        10,
                    )
                    .await
                    .unwrap();
                assert!(repository
                    .reject_https_manifest_provision(
                        "null-dns",
                        &"d".repeat(64),
                        "actor",
                        "session",
                        11
                    )
                    .await
                    .is_err());
            }
        }
    }

    #[tokio::test]
    async fn v46_dns_save_validation_conflict_and_debug_redaction() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v46-dns-validation.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x49; 32]));
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        let bytes = b"sensitive https://secret.example 192.0.2.1 token-secret".to_vec();
        let base = HttpsManifestProvisionRecord {
            provision_id: "same".into(),
            actor: "actor".into(),
            transport_session_binding: "session".into(),
            requested_url: Some("https://example.com/manifest".into()),
            requested_url_id: crate::utils::bytes_to_hex(Sha256::digest(
                "https://example.com/manifest".as_bytes(),
            )),
            final_url_id: "final".into(),
            raw_digest: crate::utils::bytes_to_hex(Sha256::digest(&bytes)),
            parsed_digest: "b".repeat(64),
            redirect_chain_digest: "c".repeat(64),
            dns_evidence_digest: Some("d".repeat(64)),
            frozen_bytes: bytes.clone(),
            expires_at_ms: 100,
            status: "saved".into(),
            created_at_ms: 1,
        };
        for digest in [
            None,
            Some("D".repeat(64)),
            Some("d".repeat(63)),
            Some("g".repeat(64)),
        ] {
            let mut record = base.clone();
            record.provision_id = format!("invalid-{}", digest.is_some());
            record.dns_evidence_digest = digest;
            assert_eq!(
                repository
                    .save_https_manifest_provision(record, &"e".repeat(64))
                    .await
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::ManifestConflict
            );
        }
        repository
            .save_https_manifest_provision(base.clone(), &"e".repeat(64))
            .await
            .unwrap();
        let mut changed = base.clone();
        changed.dns_evidence_digest = Some("f".repeat(64));
        assert_eq!(
            repository
                .save_https_manifest_provision(changed, &"e".repeat(64))
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IdempotencyConflict
        );
        let debug = format!("{:?}", base);
        assert!(
            !debug.contains("secret.example")
                && !debug.contains("192.0.2.1")
                && !debug.contains("token-secret")
        );
    }

    #[tokio::test]
    async fn v47_upgrade_adds_immutable_nullable_requested_url_and_preserves_legacy_rows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v47-upgrade.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x4a; 32],
            database_path_binding(&path).unwrap(),
        ));
        create_schema_fixture_with_version(&path, 46).await;
        {
            let pool = existing_repository_pool(&path).await;
            for (id, status) in [("legacy-saved", "saved"), ("legacy-claimed", "claimed")] {
                sqlx::query("INSERT INTO https_manifest_provisions (provision_id,actor,transport_session_binding,requested_url_id,final_url_id,raw_digest,parsed_digest,redirect_chain_digest,dns_evidence_digest,frozen_bytes,token_hash,expires_at_ms,status,created_at_ms,claimed_at_ms,claim_lease_until_ms,terminal_at_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
                    .bind(id).bind("actor").bind("session").bind("legacy-id").bind("final")
                    .bind("a".repeat(64)).bind("b".repeat(64)).bind("c".repeat(64)).bind("d".repeat(64))
                    .bind(Vec::<u8>::new()).bind(if status == "claimed" { "f".repeat(64) } else { "e".repeat(64) }).bind(100_i64).bind(status).bind(1_i64)
                    .bind((status == "claimed").then_some(2_i64))
                    .bind((status == "claimed").then_some(90_i64)).bind(None::<i64>)
                    .execute(&pool).await.unwrap();
            }
            pool.close().await;
        }
        initialize_integrity_fixture(&path, signer.clone()).await;
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        repository.verify_integrity().await.unwrap();
        let columns = sqlx::query("PRAGMA table_info('https_manifest_provisions')")
            .fetch_all(&repository.pool)
            .await
            .unwrap();
        let requested = columns
            .iter()
            .find(|row| row.get::<String, _>("name") == "requested_url")
            .unwrap();
        assert_eq!(
            requested.get::<String, _>("type").to_ascii_uppercase(),
            "TEXT"
        );
        assert_eq!(requested.get::<i64, _>("notnull"), 0);
        assert_eq!(requested.get::<i64, _>("pk"), 0);
        assert!(sqlx::query(
            "UPDATE https_manifest_provisions SET requested_url=? WHERE provision_id=?"
        )
        .bind("https://example.com/manifest")
        .bind("legacy-saved")
        .execute(&repository.pool)
        .await
        .is_err());
        assert_eq!(sqlx::query_scalar::<_, Option<String>>("SELECT requested_url FROM https_manifest_provisions WHERE provision_id='legacy-saved'").fetch_one(&repository.pool).await.unwrap(), None);

        let saved_before: (Option<String>, String, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as("SELECT requested_url,status,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='legacy-saved'").fetch_one(&repository.pool).await.unwrap();
        let checkpoint = signer.checkpoint();
        let commits = sqlite_integrity_commit_count(&path).await;
        assert_eq!(
            repository
                .claim_https_manifest_provision(
                    "legacy-saved",
                    &"e".repeat(64),
                    "actor",
                    "session",
                    10
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );
        repository.verify_integrity().await.unwrap();
        assert_eq!(sqlx::query_as::<_, (Option<String>, String, Option<i64>, Option<i64>, Option<i64>)>("SELECT requested_url,status,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='legacy-saved'").fetch_one(&repository.pool).await.unwrap(), saved_before);
        assert_eq!(signer.checkpoint(), checkpoint);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits);

        assert_eq!(
            repository
                .consume_https_manifest_provision(
                    "legacy-saved",
                    &"e".repeat(64),
                    "actor",
                    "session",
                    10
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::NotFound
        );
        repository.verify_integrity().await.unwrap();
        assert_eq!(sqlx::query_as::<_, (Option<String>, String, Option<i64>, Option<i64>, Option<i64>)>("SELECT requested_url,status,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='legacy-saved'").fetch_one(&repository.pool).await.unwrap(), saved_before);
        assert_eq!(signer.checkpoint(), checkpoint);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits);

        let finalize_manifest = ManifestRecord {
            verified: parse_manifest(REMOTE.as_bytes()).unwrap(),
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            source_metadata: ManifestSourceMetadata {
                source_ref: crate::mcp_platform::manifest::SourceRef::HttpsManifestUrl {
                    manifest_url: "https://example.com/manifest".into(),
                },
                ..Default::default()
            },
            created_at_ms: 1,
        };
        assert_eq!(
            repository
                .finalize_https_manifest_provision(
                    "legacy-saved",
                    &"e".repeat(64),
                    "actor",
                    "session",
                    10,
                    &finalize_manifest
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::NotFound
        );
        repository.verify_integrity().await.unwrap();
        assert_eq!(sqlx::query_as::<_, (Option<String>, String, Option<i64>, Option<i64>, Option<i64>)>("SELECT requested_url,status,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='legacy-saved'").fetch_one(&repository.pool).await.unwrap(), saved_before);
        assert_eq!(signer.checkpoint(), checkpoint);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits);

        assert_eq!(
            repository
                .reject_https_manifest_provision(
                    "legacy-saved",
                    &"e".repeat(64),
                    "actor",
                    "session",
                    10
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::NotFound
        );
        repository.verify_integrity().await.unwrap();
        assert_eq!(sqlx::query_as::<_, (Option<String>, String, Option<i64>, Option<i64>, Option<i64>)>("SELECT requested_url,status,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='legacy-saved'").fetch_one(&repository.pool).await.unwrap(), saved_before);
        assert_eq!(signer.checkpoint(), checkpoint);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits);

        let claimed_before: (Option<String>, String, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as("SELECT requested_url,status,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='legacy-claimed'").fetch_one(&repository.pool).await.unwrap();
        let claimed_checkpoint = signer.checkpoint();
        let claimed_commits = sqlite_integrity_commit_count(&path).await;
        assert_eq!(
            repository
                .claim_https_manifest_provision(
                    "legacy-claimed",
                    &"f".repeat(64),
                    "actor",
                    "session",
                    10
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::NotFound
        );
        repository.verify_integrity().await.unwrap();
        assert_eq!(sqlx::query_as::<_, (Option<String>, String, Option<i64>, Option<i64>, Option<i64>)>("SELECT requested_url,status,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='legacy-claimed'").fetch_one(&repository.pool).await.unwrap(), claimed_before);
        assert_eq!(signer.checkpoint(), claimed_checkpoint);
        assert_eq!(sqlite_integrity_commit_count(&path).await, claimed_commits);
        assert_eq!(
            repository
                .consume_https_manifest_provision(
                    "legacy-claimed",
                    &"f".repeat(64),
                    "actor",
                    "session",
                    10
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );
        repository.verify_integrity().await.unwrap();
        assert_eq!(sqlx::query_as::<_, (Option<String>, String, Option<i64>, Option<i64>, Option<i64>)>("SELECT requested_url,status,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='legacy-claimed'").fetch_one(&repository.pool).await.unwrap(), claimed_before);
        assert_eq!(signer.checkpoint(), claimed_checkpoint);
        assert_eq!(sqlite_integrity_commit_count(&path).await, claimed_commits);
        for (token, actor, session, expected_code) in [
            (
                &"f".repeat(64),
                "actor",
                "session",
                McpPlatformErrorCode::IntegrityError,
            ),
            (
                &"f".repeat(64),
                "wrong-actor",
                "session",
                McpPlatformErrorCode::NotFound,
            ),
            (
                &"f".repeat(64),
                "actor",
                "wrong-session",
                McpPlatformErrorCode::NotFound,
            ),
        ] {
            assert_eq!(
                repository
                    .finalize_https_manifest_provision(
                        "legacy-claimed",
                        token,
                        actor,
                        session,
                        10,
                        &finalize_manifest
                    )
                    .await
                    .unwrap_err()
                    .code(),
                expected_code
            );
        }
        repository.verify_integrity().await.unwrap();
        assert_eq!(sqlx::query_as::<_, (Option<String>, String, Option<i64>, Option<i64>, Option<i64>)>("SELECT requested_url,status,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='legacy-claimed'").fetch_one(&repository.pool).await.unwrap(), claimed_before);
        assert_eq!(signer.checkpoint(), claimed_checkpoint);
        assert_eq!(sqlite_integrity_commit_count(&path).await, claimed_commits);
        assert_eq!(
            repository
                .finalize_https_manifest_provision(
                    "legacy-claimed",
                    &"f".repeat(64),
                    "actor",
                    "session",
                    10,
                    &finalize_manifest
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );
        repository.verify_integrity().await.unwrap();
        assert_eq!(sqlx::query_as::<_, (Option<String>, String, Option<i64>, Option<i64>, Option<i64>)>("SELECT requested_url,status,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='legacy-claimed'").fetch_one(&repository.pool).await.unwrap(), claimed_before);
        assert_eq!(signer.checkpoint(), claimed_checkpoint);
        assert_eq!(sqlite_integrity_commit_count(&path).await, claimed_commits);
        repository
            .reject_https_manifest_provision(
                "legacy-claimed",
                &"f".repeat(64),
                "actor",
                "session",
                10,
            )
            .await
            .unwrap();
        repository.verify_integrity().await.unwrap();
        let rejected: (Option<String>, String, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as("SELECT requested_url,status,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='legacy-claimed'").fetch_one(&repository.pool).await.unwrap();
        assert_eq!(rejected.0, claimed_before.0);
        assert_eq!(rejected.1, "rejected");
        assert_eq!(rejected.2, claimed_before.2);
        assert_eq!(rejected.3, claimed_before.3);
        assert_eq!(rejected.4, Some(10));
        assert_ne!(signer.checkpoint(), claimed_checkpoint);
        assert!(sqlite_integrity_commit_count(&path).await > claimed_commits);
        assert_eq!(
            repository
                .reject_https_manifest_provision(
                    "legacy-claimed",
                    &"f".repeat(64),
                    "actor",
                    "session",
                    11
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::NotFound
        );
        repository.verify_integrity().await.unwrap();
    }

    #[tokio::test]
    async fn v47_canonical_requested_url_save_claim_and_consume_reject_mismatches() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v47-normal.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x4b; 32]));
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
                .await
                .unwrap();
        let url = "https://example.com/manifest";
        let token_hash = "f".repeat(64);
        let bytes = b"manifest".to_vec();
        let record = HttpsManifestProvisionRecord {
            provision_id: "normal".into(),
            actor: "actor".into(),
            transport_session_binding: "session".into(),
            requested_url: Some(url.into()),
            requested_url_id: crate::utils::bytes_to_hex(Sha256::digest(url.as_bytes())),
            final_url_id: "final".into(),
            raw_digest: crate::utils::bytes_to_hex(Sha256::digest(&bytes)),
            parsed_digest: "b".repeat(64),
            redirect_chain_digest: "c".repeat(64),
            dns_evidence_digest: Some("d".repeat(64)),
            frozen_bytes: bytes,
            expires_at_ms: 100,
            status: "saved".into(),
            created_at_ms: 1,
        };
        repository
            .save_https_manifest_provision(record.clone(), &token_hash)
            .await
            .unwrap();
        assert_eq!(
            repository
                .save_https_manifest_provision(
                    {
                        let mut r = record.clone();
                        r.provision_id = "mismatch".into();
                        r.requested_url_id = "0".repeat(64);
                        r
                    },
                    &token_hash
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::ManifestConflict
        );
        repository
            .claim_https_manifest_provision("normal", &token_hash, "actor", "session", 10)
            .await
            .unwrap();
        repository
            .consume_https_manifest_provision("normal", &token_hash, "actor", "session", 20)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM https_manifest_provisions WHERE provision_id='normal'"
            )
            .fetch_one(&repository.pool)
            .await
            .unwrap(),
            "consumed"
        );
    }

    #[tokio::test]
    async fn v47_claim_consume_finalize_filters_keep_valid_saved_record_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v47-filters.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x4c; 32]));
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let url = "https://example.com/manifest";
        let token_hash = "f".repeat(64);
        let record = HttpsManifestProvisionRecord {
            provision_id: "filter-case".into(),
            actor: "actor".into(),
            transport_session_binding: "session".into(),
            requested_url: Some(url.into()),
            requested_url_id: crate::utils::bytes_to_hex(Sha256::digest(url.as_bytes())),
            final_url_id: "final".into(),
            raw_digest: crate::utils::bytes_to_hex(Sha256::digest(b"fixture")),
            parsed_digest: "b".repeat(64),
            redirect_chain_digest: "c".repeat(64),
            dns_evidence_digest: Some("d".repeat(64)),
            frozen_bytes: b"fixture".to_vec(),
            expires_at_ms: 100,
            status: "saved".into(),
            created_at_ms: 1,
        };
        repository
            .save_https_manifest_provision(record, &token_hash)
            .await
            .unwrap();
        let manifest = ManifestRecord {
            verified: parse_manifest(REMOTE.as_bytes()).unwrap(),
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            source_metadata: ManifestSourceMetadata {
                source_ref: crate::mcp_platform::manifest::SourceRef::HttpsManifestUrl {
                    manifest_url: url.into(),
                },
                ..Default::default()
            },
            created_at_ms: 1,
        };
        for (actor, binding, token, now_ms) in [
            ("other", "session", token_hash.as_str(), 10),
            ("actor", "other", token_hash.as_str(), 10),
            ("actor", "session", "0", 10),
            ("actor", "session", token_hash.as_str(), 100),
        ] {
            let before: (String, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as("SELECT status,claim_lease_until_ms,claimed_at_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='filter-case'").fetch_one(&repository.pool).await.unwrap();
            let checkpoint = signer.checkpoint();
            let commits = sqlite_integrity_commit_count(&path).await;
            assert_eq!(
                repository
                    .claim_https_manifest_provision("filter-case", token, actor, binding, now_ms)
                    .await
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::NotFound
            );
            assert_eq!(
                repository
                    .consume_https_manifest_provision("filter-case", token, actor, binding, now_ms)
                    .await
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::NotFound
            );
            assert_eq!(
                repository
                    .finalize_https_manifest_provision(
                        "filter-case",
                        token,
                        actor,
                        binding,
                        now_ms,
                        &manifest
                    )
                    .await
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::NotFound
            );
            assert_eq!(sqlx::query_as::<_, (String, Option<i64>, Option<i64>, Option<i64>)>("SELECT status,claim_lease_until_ms,claimed_at_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='filter-case'").fetch_one(&repository.pool).await.unwrap(), before);
            assert_eq!(signer.checkpoint(), checkpoint);
            assert_eq!(sqlite_integrity_commit_count(&path).await, commits);
        }
    }

    #[tokio::test]
    async fn v47_finalize_identity_mismatch_is_unenumerable_and_has_no_side_effects() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v47-finalize-identity-mismatch.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x4d; 32]));
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let url = "https://example.com/manifest";
        let token_hash = "f".repeat(64);
        let record = HttpsManifestProvisionRecord {
            provision_id: "claimed-case".into(),
            actor: "actor".into(),
            transport_session_binding: "session".into(),
            requested_url: Some(url.into()),
            requested_url_id: crate::utils::bytes_to_hex(Sha256::digest(url.as_bytes())),
            final_url_id: "final".into(),
            raw_digest: crate::utils::bytes_to_hex(Sha256::digest(b"fixture")),
            parsed_digest: "b".repeat(64),
            redirect_chain_digest: "c".repeat(64),
            dns_evidence_digest: Some("d".repeat(64)),
            frozen_bytes: b"fixture".to_vec(),
            expires_at_ms: 100,
            status: "saved".into(),
            created_at_ms: 1,
        };
        repository
            .save_https_manifest_provision(record, &token_hash)
            .await
            .unwrap();
        repository
            .claim_https_manifest_provision("claimed-case", &token_hash, "actor", "session", 10)
            .await
            .unwrap();
        let before: (String, Option<i64>, Option<i64>) = sqlx::query_as("SELECT status,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='claimed-case'").fetch_one(&repository.pool).await.unwrap();
        let manifest = ManifestRecord {
            verified: parse_manifest(REMOTE.as_bytes()).unwrap(),
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            source_metadata: ManifestSourceMetadata {
                source_ref: crate::mcp_platform::manifest::SourceRef::HttpsManifestUrl {
                    manifest_url: "https://example.com/other".into(),
                },
                ..Default::default()
            },
            created_at_ms: 1,
        };
        let checkpoint = signer.checkpoint();
        let commits = sqlite_integrity_commit_count(&path).await;
        assert_eq!(
            repository
                .finalize_https_manifest_provision(
                    "claimed-case",
                    &token_hash,
                    "actor",
                    "session",
                    20,
                    &manifest
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::NotFound
        );
        let invalid_source = ManifestRecord {
            source_metadata: ManifestSourceMetadata::default(),
            ..manifest
        };
        assert_eq!(
            repository
                .finalize_https_manifest_provision(
                    "claimed-case",
                    &token_hash,
                    "actor",
                    "session",
                    20,
                    &invalid_source
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::ManifestConflict
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM manifest_blobs")
                .fetch_one(&repository.pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(sqlx::query_as::<_, (String, Option<i64>, Option<i64>)>("SELECT status,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='claimed-case'").fetch_one(&repository.pool).await.unwrap(), before);
        repository.verify_integrity().await.unwrap();
        assert_eq!(signer.checkpoint(), checkpoint);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits);
    }

    #[tokio::test]
    async fn v47_finalize_storage_url_integrity_matrix_rejects_without_mutation() {
        let cases = [
            (
                "noncanonical-url",
                "https://example.com:443/manifest",
                "hash",
            ),
            (
                "url-id-hash-mismatch",
                "https://example.com/manifest",
                "hash",
            ),
            (
                "url-id-uppercase",
                "https://example.com/manifest",
                "uppercase",
            ),
            ("url-id-nonhex", "https://example.com/manifest", "nonhex"),
            (
                "url-id-wrong-length",
                "https://example.com/manifest",
                "short",
            ),
        ];
        let canonical_url = "https://example.com/manifest";
        let token_hash = "f".repeat(64);
        let requested_hash = crate::utils::bytes_to_hex(Sha256::digest(canonical_url.as_bytes()));

        for (case_name, stored_url, id_case) in cases {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(format!("v47-{case_name}.db"));
            let signer = Arc::new(InMemoryIntegritySigner::new_for_testing_with_path_binding(
                [0x4e; 32],
                database_path_binding(&path).unwrap(),
            ));
            create_schema_fixture_with_version(&path, 46).await;
            let pool = existing_repository_pool(&path).await;
            migrate_through_version(&pool, 47).await.unwrap();
            let requested_url_id = match id_case {
                "hash" => "0".repeat(64),
                "uppercase" => requested_hash.to_ascii_uppercase(),
                "nonhex" => format!("{}g", "0".repeat(63)),
                "short" => "0".repeat(63),
                _ => requested_hash.clone(),
            };
            sqlx::query("INSERT INTO https_manifest_provisions (provision_id,actor,transport_session_binding,requested_url,requested_url_id,final_url_id,raw_digest,parsed_digest,redirect_chain_digest,dns_evidence_digest,frozen_bytes,token_hash,expires_at_ms,status,created_at_ms,claimed_at_ms,claim_lease_until_ms,terminal_at_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
                .bind("storage-corruption").bind("actor").bind("session").bind(stored_url)
                .bind(requested_url_id).bind("final").bind(crate::utils::bytes_to_hex(Sha256::digest(b"fixture"))).bind("b".repeat(64))
                .bind("c".repeat(64)).bind("d".repeat(64)).bind(b"fixture".to_vec()).bind(&token_hash)
                .bind(100_i64).bind("claimed").bind(1_i64).bind(2_i64).bind(90_i64).bind(None::<i64>)
                .execute(&pool).await.unwrap();
            pool.close().await;
            initialize_integrity_fixture(&path, signer.clone()).await;
            let repository =
                SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                    .await
                    .unwrap();
            let manifest = ManifestRecord {
                verified: parse_manifest(REMOTE.as_bytes()).unwrap(),
                proof: ManifestProof::LocalBytes,
                trust_tier: TrustTier::Local,
                source_metadata: ManifestSourceMetadata {
                    source_ref: crate::mcp_platform::manifest::SourceRef::HttpsManifestUrl {
                        manifest_url: canonical_url.into(),
                    },
                    ..Default::default()
                },
                created_at_ms: 1,
            };
            let before: (Option<String>, Option<String>, String, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as("SELECT requested_url,requested_url_id,status,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='storage-corruption'").fetch_one(&repository.pool).await.unwrap();
            let manifest_count =
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM manifest_blobs")
                    .fetch_one(&repository.pool)
                    .await
                    .unwrap();
            let checkpoint = signer.checkpoint();
            let commits = sqlite_integrity_commit_count(&path).await;
            let error = repository
                .finalize_https_manifest_provision(
                    "storage-corruption",
                    &token_hash,
                    "actor",
                    "session",
                    20,
                    &manifest,
                )
                .await
                .unwrap_err();
            assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM manifest_blobs")
                    .fetch_one(&repository.pool)
                    .await
                    .unwrap(),
                manifest_count
            );
            assert_eq!(sqlx::query_as::<_, (Option<String>, Option<String>, String, Option<i64>, Option<i64>, Option<i64>)>("SELECT requested_url,requested_url_id,status,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='storage-corruption'").fetch_one(&repository.pool).await.unwrap(), before);
            assert_eq!(signer.checkpoint(), checkpoint);
            assert_eq!(sqlite_integrity_commit_count(&path).await, commits);
            repository
                .reject_https_manifest_provision(
                    "storage-corruption",
                    &token_hash,
                    "actor",
                    "session",
                    20,
                )
                .await
                .unwrap();
            assert_eq!(sqlx::query_scalar::<_, String>("SELECT status FROM https_manifest_provisions WHERE provision_id='storage-corruption'").fetch_one(&repository.pool).await.unwrap(), "rejected");
            assert_eq!(
                repository
                    .reject_https_manifest_provision(
                        "storage-corruption",
                        &token_hash,
                        "actor",
                        "session",
                        21
                    )
                    .await
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::NotFound
            );
        }
    }

    #[tokio::test]
    async fn consumed_https_manifest_lookup_is_bound_and_read_only() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("consumed-lookup.db");
        let signer = Arc::new(InMemoryIntegritySigner::new_for_testing([0x4f; 32]));
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let url = "https://example.com/manifest";
        let bytes = b"manifest".to_vec();
        let token_hash = "f".repeat(64);
        let record = HttpsManifestProvisionRecord {
            provision_id: "lookup".into(),
            actor: "actor".into(),
            transport_session_binding: "session".into(),
            requested_url: Some(url.into()),
            requested_url_id: crate::utils::bytes_to_hex(Sha256::digest(url.as_bytes())),
            final_url_id: "final".into(),
            raw_digest: crate::utils::bytes_to_hex(Sha256::digest(&bytes)),
            parsed_digest: "b".repeat(64),
            redirect_chain_digest: "c".repeat(64),
            dns_evidence_digest: Some("d".repeat(64)),
            frozen_bytes: bytes,
            expires_at_ms: 100,
            status: "saved".into(),
            created_at_ms: 1,
        };
        repository
            .save_https_manifest_provision(record.clone(), &token_hash)
            .await
            .unwrap();
        for status in ["saved", "claimed", "rejected"] {
            if status == "claimed" {
                repository
                    .claim_https_manifest_provision("lookup", &token_hash, "actor", "session", 10)
                    .await
                    .unwrap();
            } else if status == "rejected" {
                repository
                    .reject_https_manifest_provision("lookup", &token_hash, "actor", "session", 10)
                    .await
                    .unwrap();
            }
            assert_eq!(
                repository
                    .get_consumed_https_manifest_provision("lookup", "actor", "session")
                    .await
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::NotFound
            );
            if status == "saved" {
                break;
            }
        }
        repository
            .save_https_manifest_provision(
                {
                    let mut r = record;
                    r.provision_id = "consumed".into();
                    r
                },
                &token_hash,
            )
            .await
            .unwrap();
        repository
            .claim_https_manifest_provision("consumed", &token_hash, "actor", "session", 10)
            .await
            .unwrap();
        repository
            .consume_https_manifest_provision("consumed", &token_hash, "actor", "session", 20)
            .await
            .unwrap();
        let before = sqlx::query_as::<_, (String, Option<i64>, Option<i64>, Option<i64>)>("SELECT status,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='consumed'").fetch_one(&repository.pool).await.unwrap();
        let checkpoint = signer.checkpoint();
        let commits = sqlite_integrity_commit_count(&path).await;
        let found = repository
            .get_consumed_https_manifest_provision("consumed", "actor", "session")
            .await
            .unwrap();
        assert_eq!(found.status, "consumed");
        assert_eq!(found.frozen_bytes, b"manifest");
        assert_eq!(
            repository
                .get_consumed_https_manifest_provision("consumed", "wrong", "session")
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::NotFound
        );
        assert_eq!(
            repository
                .get_consumed_https_manifest_provision("consumed", "actor", "wrong")
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::NotFound
        );
        assert_eq!(sqlx::query_as::<_, (String, Option<i64>, Option<i64>, Option<i64>)>("SELECT status,claimed_at_ms,claim_lease_until_ms,terminal_at_ms FROM https_manifest_provisions WHERE provision_id='consumed'").fetch_one(&repository.pool).await.unwrap(), before);
        assert_eq!(signer.checkpoint(), checkpoint);
        assert_eq!(sqlite_integrity_commit_count(&path).await, commits);
    }

    #[tokio::test]
    async fn consumed_https_manifest_lookup_fails_closed_for_legacy_corruption() {
        let cases = [
            ("null-url", None, "valid", false),
            (
                "noncanonical-url",
                Some("https://example.com:443/manifest"),
                "valid",
                false,
            ),
            ("invalid-url", Some("not-a-url"), "valid", false),
            (
                "mismatched-url-id",
                Some("https://example.com/manifest"),
                "mismatch",
                false,
            ),
            (
                "invalid-url-id",
                Some("https://example.com/manifest"),
                "invalid",
                false,
            ),
            (
                "null-dns",
                Some("https://example.com/manifest"),
                "valid",
                true,
            ),
        ];
        let canonical_url = "https://example.com/manifest";
        let canonical_url_id = crate::utils::bytes_to_hex(Sha256::digest(canonical_url.as_bytes()));
        let token_hash = "f".repeat(64);

        for (case_name, requested_url, requested_url_id_case, null_dns) in cases {
            let directory = tempfile::tempdir().unwrap();
            let path = directory
                .path()
                .join(format!("consumed-legacy-{case_name}.db"));
            let signer = Arc::new(InMemoryIntegritySigner::new_for_testing_with_path_binding(
                [0x50; 32],
                database_path_binding(&path).unwrap(),
            ));
            create_schema_fixture_with_version(&path, 46).await;
            let pool = existing_repository_pool(&path).await;
            migrate_through_version(&pool, 47).await.unwrap();
            let requested_url_id = match requested_url_id_case {
                "valid" => canonical_url_id.clone(),
                "mismatch" => "0".repeat(64),
                "invalid" => "not-a-digest".to_string(),
                _ => unreachable!(),
            };
            sqlx::query("INSERT INTO https_manifest_provisions (provision_id,actor,transport_session_binding,requested_url,requested_url_id,final_url_id,raw_digest,parsed_digest,redirect_chain_digest,dns_evidence_digest,frozen_bytes,token_hash,expires_at_ms,status,created_at_ms,terminal_at_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
                .bind("legacy-consumed").bind("actor").bind("session").bind(requested_url)
                .bind(requested_url_id).bind("final").bind("a".repeat(64)).bind("b".repeat(64))
                .bind(if null_dns { None::<String> } else { Some("d".repeat(64)) })
                .bind(b"fixture".to_vec()).bind(&token_hash).bind(100_i64).bind("consumed").bind(1_i64).bind(20_i64)
                .execute(&pool).await.unwrap();
            pool.close().await;
            initialize_integrity_fixture(&path, signer.clone()).await;
            let repository =
                SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                    .await
                    .unwrap();
            let before: (Option<String>, Option<String>, Option<String>, String, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as("SELECT requested_url,requested_url_id,dns_evidence_digest,status,terminal_at_ms,claimed_at_ms,claim_lease_until_ms FROM https_manifest_provisions WHERE provision_id='legacy-consumed'").fetch_one(&repository.pool).await.unwrap();
            let checkpoint = signer.checkpoint();
            let commits = sqlite_integrity_commit_count(&path).await;
            let error = repository
                .get_consumed_https_manifest_provision("legacy-consumed", "actor", "session")
                .await
                .unwrap_err();
            assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
            assert_eq!(sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>, String, Option<i64>, Option<i64>, Option<i64>)>("SELECT requested_url,requested_url_id,dns_evidence_digest,status,terminal_at_ms,claimed_at_ms,claim_lease_until_ms FROM https_manifest_provisions WHERE provision_id='legacy-consumed'").fetch_one(&repository.pool).await.unwrap(), before);
            assert_eq!(signer.checkpoint(), checkpoint);
            assert_eq!(sqlite_integrity_commit_count(&path).await, commits);
        }
    }
}
