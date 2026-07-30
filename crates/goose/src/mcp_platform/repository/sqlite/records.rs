use serde::de::DeserializeOwned;
use serde::Serialize;
use sqlx::{Pool, Row, Sqlite, Transaction};

use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::manifest::{
    digest_serializable, parse_manifest, ManifestProof, ManifestSourceMetadata, SourceImportKind,
};
use crate::mcp_platform::policy::{PlanOperation, PolicyDecision};
use crate::mcp_platform::repository::{
    AuditEventRecord, AuditEventType, AuditPayload, ConfirmationEvidence, ManagedMcpRecord,
    ManagedVersionRecord, ManifestRecord, PlanRecord, PlanTarget, SavePlan, TaskRecord,
    TaskStepRecord,
};
use crate::mcp_platform::task::{RecoveryDecision, RedactedError, TaskStatus, TaskStepStatus};
use crate::mcp_platform::TrustTier;

pub(super) async fn schema_objects(
    pool: &Pool<Sqlite>,
    kind: &str,
) -> McpPlatformResult<Vec<String>> {
    let rows = sqlx::query(
        "SELECT name FROM sqlite_master WHERE type = ? AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .bind(kind)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx)?;
    rows.iter()
        .map(|row| row.try_get("name").map_err(map_sqlx))
        .collect()
}

pub(super) async fn find_managed_by_identity(
    tx: &mut Transaction<'_, Sqlite>,
    mcp_id: &str,
    installation_scope: &str,
) -> McpPlatformResult<Option<String>> {
    sqlx::query_scalar(
        "SELECT managed_mcp_id FROM managed_mcps WHERE mcp_id = ? AND installation_scope = ?",
    )
    .bind(mcp_id)
    .bind(installation_scope)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx)
}

pub(super) fn decode_manifest_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<ManifestRecord> {
    let stored_digest: String = row.try_get("manifest_digest").map_err(map_sqlx)?;
    let stored_id: String = row.try_get("mcp_id").map_err(map_sqlx)?;
    let stored_version: String = row.try_get("version").map_err(map_sqlx)?;
    let canonical_bytes: Vec<u8> = row.try_get("canonical_bytes").map_err(map_sqlx)?;
    let verified = parse_manifest(&canonical_bytes).map_err(|_| integrity_error())?;
    if verified.digest() != stored_digest
        || verified.manifest().id != stored_id
        || verified.manifest().version.as_str() != stored_version
    {
        return Err(integrity_error());
    }
    let proof: ManifestProof = decode(&row.try_get::<String, _>("proof_json").map_err(map_sqlx)?)?;
    validate_manifest_proof(&proof, verified.digest()).map_err(|_| integrity_error())?;
    let trust_tier: TrustTier = decode(
        &row.try_get::<String, _>("trust_tier_json")
            .map_err(map_sqlx)?,
    )?;
    let source_metadata = decode_manifest_source_metadata_row(row)?;
    source_metadata
        .validate_trust_tier(trust_tier)
        .map_err(|_| integrity_error())?;
    Ok(ManifestRecord {
        verified,
        proof,
        trust_tier,
        source_metadata,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
    })
}

pub(super) fn decode_managed_row(
    row: &sqlx::sqlite::SqliteRow,
    versions: Vec<ManagedVersionRecord>,
) -> McpPlatformResult<ManagedMcpRecord> {
    Ok(ManagedMcpRecord {
        managed_mcp_id: row.try_get("managed_mcp_id").map_err(map_sqlx)?,
        mcp_id: row.try_get("mcp_id").map_err(map_sqlx)?,
        installation_scope: row.try_get("installation_scope").map_err(map_sqlx)?,
        state: decode(&row.try_get::<String, _>("state_json").map_err(map_sqlx)?)?,
        revision: row.try_get("revision").map_err(map_sqlx)?,
        versions,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
        updated_at_ms: row.try_get("updated_at_ms").map_err(map_sqlx)?,
    })
}

pub(super) fn decode_managed_version_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<ManagedVersionRecord> {
    Ok(ManagedVersionRecord {
        version: row.try_get("version").map_err(map_sqlx)?,
        manifest_digest: row.try_get("manifest_digest").map_err(map_sqlx)?,
        installation_root: row.try_get("installation_root").map_err(map_sqlx)?,
        verified: row.try_get::<bool, _>("verified").map_err(map_sqlx)?,
        active: row.try_get::<bool, _>("active").map_err(map_sqlx)?,
        adapter_evidence: decode_optional(
            row.try_get::<Option<String>, _>("adapter_evidence_json")
                .map_err(map_sqlx)?,
        )?,
        materialized_tree_digest: row.try_get("materialized_tree_digest").map_err(map_sqlx)?,
        supply_chain_evidence: decode_optional(
            row.try_get::<Option<String>, _>("supply_chain_evidence_json")
                .map_err(map_sqlx)?,
        )?,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
    })
}

pub(super) async fn validate_plan_manifest(
    tx: &mut Transaction<'_, Sqlite>,
    plan: &crate::mcp_platform::plan::InstallationPlan,
) -> McpPlatformResult<()> {
    let row = sqlx::query(
        r#"SELECT manifest_digest, mcp_id, version, canonical_bytes, proof_json,
            trust_tier_json, source_ref_json, import_kind, release_id,
            origin_provenance_json, update_channel_json, created_at_ms
            FROM manifest_blobs WHERE manifest_digest = ?"#,
    )
    .bind(plan.manifest_digest())
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx)?
    .ok_or_else(not_found)?;
    let manifest = decode_manifest_row(&row)?;
    if manifest.verified.digest() != plan.manifest_digest()
        || manifest.verified.manifest().id != plan.manifest_id()
        || manifest.verified.manifest().version.as_str() != plan.manifest_version()
    {
        return Err(integrity_error());
    }
    Ok(())
}

#[derive(Serialize)]
struct PlanEnvelopeDigestContent<'a> {
    plan_id: &'a str,
    idempotency_key: &'a str,
    plan_digest: &'a str,
    manifest_digest: &'a str,
    operation: PlanOperation,
    target: &'a PlanTarget,
    policy_evidence: &'a PolicyDecision,
    confirmation_evidence: &'a ConfirmationEvidence,
    expires_at_ms: i64,
    actor: &'a str,
    created_at_ms: i64,
}

pub(super) fn plan_envelope_digest_for_save(input: &SavePlan<'_>) -> McpPlatformResult<String> {
    digest_serializable(&PlanEnvelopeDigestContent {
        plan_id: input.plan_id,
        idempotency_key: input.idempotency_key,
        plan_digest: input.plan.plan_digest(),
        manifest_digest: input.plan.manifest_digest(),
        operation: input.plan.operation(),
        target: input.target,
        policy_evidence: input.policy_evidence,
        confirmation_evidence: input.confirmation_evidence,
        expires_at_ms: input.expires_at_ms,
        actor: input.actor,
        created_at_ms: input.created_at_ms,
    })
}

fn plan_envelope_digest_for_record(record: &PlanRecord) -> McpPlatformResult<String> {
    digest_serializable(&PlanEnvelopeDigestContent {
        plan_id: &record.plan_id,
        idempotency_key: &record.idempotency_key,
        plan_digest: record.plan.plan_digest(),
        manifest_digest: record.plan.manifest_digest(),
        operation: record.plan.operation(),
        target: &record.target,
        policy_evidence: &record.policy_evidence,
        confirmation_evidence: &record.confirmation_evidence,
        expires_at_ms: record.expires_at_ms,
        actor: &record.actor,
        created_at_ms: record.created_at_ms,
    })
}

pub(super) fn validate_plan_evidence(
    plan: &crate::mcp_platform::plan::InstallationPlan,
    evidence: &ConfirmationEvidence,
    created_at_ms: i64,
    expires_at_ms: i64,
) -> McpPlatformResult<()> {
    if expires_at_ms <= created_at_ms {
        return Err(integrity_error());
    }
    match evidence {
        ConfirmationEvidence::NotRequired if !plan.required_confirmations().is_empty() => {
            Err(integrity_error())
        }
        ConfirmationEvidence::Confirmed {
            actor,
            confirmed_at_ms,
        } if actor.is_empty()
            || *confirmed_at_ms < created_at_ms
            || *confirmed_at_ms > expires_at_ms =>
        {
            Err(integrity_error())
        }
        _ => Ok(()),
    }
}

pub(super) fn decode_plan_row(row: &sqlx::sqlite::SqliteRow) -> McpPlatformResult<PlanRecord> {
    let plan: crate::mcp_platform::plan::InstallationPlan =
        decode(&row.try_get::<String, _>("plan_json").map_err(map_sqlx)?)?;
    plan.verify_integrity().map_err(|_| integrity_error())?;
    let stored_digest: String = row.try_get("plan_digest").map_err(map_sqlx)?;
    let stored_manifest_digest: String = row.try_get("manifest_digest").map_err(map_sqlx)?;
    let stored_operation: String = row.try_get("operation").map_err(map_sqlx)?;
    let policy_evidence = decode(
        &row.try_get::<String, _>("policy_evidence_json")
            .map_err(map_sqlx)?,
    )?;
    if stored_digest != plan.plan_digest()
        || stored_manifest_digest != plan.manifest_digest()
        || stored_operation != plan.operation().as_str()
        || &policy_evidence != plan.policy()
    {
        return Err(integrity_error());
    }
    let target: PlanTarget = decode(&row.try_get::<String, _>("target_json").map_err(map_sqlx)?)?;
    if target.source_context != plan.source_context().cloned() {
        return Err(integrity_error());
    }
    let record = PlanRecord {
        plan_id: row.try_get("plan_id").map_err(map_sqlx)?,
        idempotency_key: row.try_get("idempotency_key").map_err(map_sqlx)?,
        envelope_digest: row.try_get("envelope_digest").map_err(map_sqlx)?,
        plan,
        target,
        policy_evidence,
        confirmation_evidence: decode(
            &row.try_get::<String, _>("confirmation_evidence_json")
                .map_err(map_sqlx)?,
        )?,
        expires_at_ms: row.try_get("expires_at_ms").map_err(map_sqlx)?,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
        actor: row.try_get("actor").map_err(map_sqlx)?,
    };
    validate_plan_evidence(
        &record.plan,
        &record.confirmation_evidence,
        record.created_at_ms,
        record.expires_at_ms,
    )?;
    if matches!(
        &record.confirmation_evidence,
        ConfirmationEvidence::Confirmed { actor, .. } if actor != &record.actor
    ) {
        return Err(integrity_error());
    }
    if plan_envelope_digest_for_record(&record)? != record.envelope_digest {
        return Err(integrity_error());
    }
    Ok(record)
}

pub(super) async fn fetch_valid_plan(
    tx: &mut Transaction<'_, Sqlite>,
    plan_id: &str,
) -> McpPlatformResult<PlanRecord> {
    let row = sqlx::query(
        r#"SELECT plan_id, plan_digest, envelope_digest, manifest_digest, operation,
            target_json, plan_json, policy_evidence_json, confirmation_evidence_json,
            expires_at_ms, idempotency_key, actor, created_at_ms
            FROM install_plans WHERE plan_id = ?"#,
    )
    .bind(plan_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx)?
    .ok_or_else(not_found)?;
    let plan = decode_plan_row(&row)?;
    validate_plan_manifest(tx, &plan.plan).await?;
    if plan.target.mcp_id != plan.plan.manifest_id()
        || plan.target.version != plan.plan.manifest_version()
    {
        return Err(integrity_error());
    }
    Ok(plan)
}

pub(super) async fn fetch_task(
    tx: &mut Transaction<'_, Sqlite>,
    task_id: &str,
) -> McpPlatformResult<TaskRecord> {
    let row = sqlx::query("SELECT * FROM tasks WHERE task_id = ?")
        .bind(task_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
    decode_task_row(&row)
}

pub(super) fn decode_task_row(row: &sqlx::sqlite::SqliteRow) -> McpPlatformResult<TaskRecord> {
    let progress: i64 = row.try_get("progress").map_err(map_sqlx)?;
    Ok(TaskRecord {
        task_id: row.try_get("task_id").map_err(map_sqlx)?,
        plan_id: row.try_get("plan_id").map_err(map_sqlx)?,
        plan_digest: row.try_get("plan_digest").map_err(map_sqlx)?,
        operation: decode_database_enum(&row.try_get::<String, _>("operation").map_err(map_sqlx)?)?,
        idempotency_key: row.try_get("idempotency_key").map_err(map_sqlx)?,
        status: decode_database_enum(&row.try_get::<String, _>("status").map_err(map_sqlx)?)?,
        actor: row.try_get("actor").map_err(map_sqlx)?,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
        updated_at_ms: row.try_get("updated_at_ms").map_err(map_sqlx)?,
        heartbeat_at_ms: row.try_get("heartbeat_at_ms").map_err(map_sqlx)?,
        progress: u8::try_from(progress).map_err(|_| integrity_error())?,
        step_cursor: row.try_get("step_cursor").map_err(map_sqlx)?,
        adapter_evidence: decode_optional(
            row.try_get::<Option<String>, _>("adapter_evidence_json")
                .map_err(map_sqlx)?,
        )?,
        redacted_error: decode_optional(
            row.try_get::<Option<String>, _>("redacted_error_json")
                .map_err(map_sqlx)?,
        )?,
        rollback_status: decode_database_enum(
            &row.try_get::<String, _>("rollback_status")
                .map_err(map_sqlx)?,
        )?,
        rollback_evidence: decode_optional(
            row.try_get::<Option<String>, _>("rollback_evidence_json")
                .map_err(map_sqlx)?,
        )?,
        revision: row.try_get("revision").map_err(map_sqlx)?,
        event_sequence: row.try_get("event_sequence").map_err(map_sqlx)?,
        owner_id: row.try_get("owner_id").map_err(map_sqlx)?,
        lease_expires_at_ms: row.try_get("lease_expires_at_ms").map_err(map_sqlx)?,
        attempt_count: row.try_get("attempt_count").map_err(map_sqlx)?,
    })
}

pub(super) async fn fetch_task_step(
    tx: &mut Transaction<'_, Sqlite>,
    signer: &dyn super::integrity::IntegritySigner,
    task_id: &str,
    ordinal: i64,
) -> McpPlatformResult<TaskStepRecord> {
    let row = sqlx::query("SELECT * FROM task_steps WHERE task_id = ? AND ordinal = ?")
        .bind(task_id)
        .bind(ordinal)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
    let step = decode_task_step_row(&row)?;
    verify_task_step_compensation_binding(tx, signer, &step, &row).await?;
    Ok(step)
}

pub(super) fn task_step_compensation_payload(
    task: &TaskRecord,
    step: &TaskStepRecord,
    compensation_json: &str,
    evidence_json: Option<&str>,
) -> Vec<u8> {
    super::integrity::canonical_fields(&[
        b"task-step-state-v2",
        step.task_id.as_bytes(),
        step.ordinal.to_string().as_bytes(),
        step.idempotency_token.as_bytes(),
        step.adapter_id.as_bytes(),
        step.adapter_version.as_bytes(),
        task.plan_id.as_bytes(),
        task.plan_digest.as_bytes(),
        task.operation.as_str().as_bytes(),
        compensation_json.as_bytes(),
        step.status.as_str().as_bytes(),
        if evidence_json.is_some() { b"1" } else { b"0" },
        evidence_json.unwrap_or_default().as_bytes(),
        step.started_at_ms
            .unwrap_or_default()
            .to_string()
            .as_bytes(),
        step.committed_at_ms
            .unwrap_or_default()
            .to_string()
            .as_bytes(),
        step.compensation_status.as_str().as_bytes(),
        step.compensation_started_at_ms
            .unwrap_or_default()
            .to_string()
            .as_bytes(),
        step.compensation_committed_at_ms
            .unwrap_or_default()
            .to_string()
            .as_bytes(),
    ])
}

pub(super) fn task_step_history_compensation_payload(
    task: &TaskRecord,
    attempt: i64,
    step: &TaskStepRecord,
    compensation_json: &str,
) -> Vec<u8> {
    let step_payload = task_step_compensation_payload(
        task,
        step,
        compensation_json,
        encode_optional(step.evidence.as_ref())
            .ok()
            .flatten()
            .as_deref(),
    );
    super::integrity::canonical_fields(&[
        b"task-step-compensation-history-v1",
        attempt.to_string().as_bytes(),
        &step_payload,
    ])
}

pub(super) async fn verify_task_step_compensation_binding(
    tx: &mut Transaction<'_, Sqlite>,
    signer: &dyn super::integrity::IntegritySigner,
    step: &TaskStepRecord,
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<()> {
    let compensation_json = row
        .try_get::<String, _>("compensation_json")
        .map_err(map_sqlx)?;
    let evidence_json = row
        .try_get::<Option<String>, _>("evidence_json")
        .map_err(map_sqlx)?;
    let task = fetch_task(tx, &step.task_id).await?;
    let payload =
        task_step_compensation_payload(&task, step, &compensation_json, evidence_json.as_deref());
    let actual = sqlx::query_scalar::<_, String>(
        "SELECT mac FROM task_step_compensation_bindings WHERE task_id = ? AND ordinal = ?",
    )
    .bind(&step.task_id)
    .bind(step.ordinal)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx)?
    .ok_or_else(integrity_error)?;
    if signer
        .verify("task-step-compensation", &payload, &actual)
        .is_ok()
    {
        return Ok(());
    }
    Err(integrity_error())
}

pub(super) fn managed_projection_integrity_payload(
    managed_mcp_id: &str,
    link_key: &str,
    projection_digest: &str,
    revision: i64,
    plan_id: Option<&str>,
    manifest_digest: Option<&str>,
    owner_task_id: Option<&str>,
) -> Vec<u8> {
    super::integrity::canonical_fields(&[
        b"managed-projection-v1",
        managed_mcp_id.as_bytes(),
        link_key.as_bytes(),
        projection_digest.as_bytes(),
        revision.to_string().as_bytes(),
        if plan_id.is_some() { b"1" } else { b"0" },
        plan_id.unwrap_or_default().as_bytes(),
        if manifest_digest.is_some() {
            b"1"
        } else {
            b"0"
        },
        manifest_digest.unwrap_or_default().as_bytes(),
        if owner_task_id.is_some() { b"1" } else { b"0" },
        owner_task_id.unwrap_or_default().as_bytes(),
    ])
}

#[allow(clippy::too_many_arguments)]
pub(super) fn projection_writer_payload(
    managed_mcp_id: &str,
    task_id: &str,
    plan_id: &str,
    plan_digest: &str,
    link_key: &str,
    manifest_digest: &str,
    projection_digest: &str,
    lifecycle_acquired_at_ms: i64,
    worker_owner_id: &str,
    worker_lease_expires_at_ms: i64,
    step_ordinal: i64,
    step_token: &str,
) -> Vec<u8> {
    super::integrity::canonical_fields(&[
        b"projection-writer-authorization-v1",
        managed_mcp_id.as_bytes(),
        task_id.as_bytes(),
        plan_id.as_bytes(),
        plan_digest.as_bytes(),
        link_key.as_bytes(),
        manifest_digest.as_bytes(),
        projection_digest.as_bytes(),
        lifecycle_acquired_at_ms.to_string().as_bytes(),
        worker_owner_id.as_bytes(),
        worker_lease_expires_at_ms.to_string().as_bytes(),
        step_ordinal.to_string().as_bytes(),
        step_token.as_bytes(),
    ])
}

pub(super) fn decode_task_step_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<TaskStepRecord> {
    Ok(TaskStepRecord {
        task_id: row.try_get("task_id").map_err(map_sqlx)?,
        ordinal: row.try_get("ordinal").map_err(map_sqlx)?,
        status: decode_database_enum(&row.try_get::<String, _>("status").map_err(map_sqlx)?)?,
        idempotency_token: row.try_get("idempotency_token").map_err(map_sqlx)?,
        compensation: decode(
            &row.try_get::<String, _>("compensation_json")
                .map_err(map_sqlx)?,
        )?,
        evidence: decode_optional(
            row.try_get::<Option<String>, _>("evidence_json")
                .map_err(map_sqlx)?,
        )?,
        started_at_ms: row.try_get("started_at_ms").map_err(map_sqlx)?,
        committed_at_ms: row.try_get("committed_at_ms").map_err(map_sqlx)?,
        adapter_id: row.try_get("adapter_id").map_err(map_sqlx)?,
        adapter_version: row.try_get("adapter_version").map_err(map_sqlx)?,
        compensation_status: decode_database_enum(
            &row.try_get::<String, _>("compensation_status")
                .map_err(map_sqlx)?,
        )?,
        compensation_started_at_ms: row
            .try_get("compensation_started_at_ms")
            .map_err(map_sqlx)?,
        compensation_committed_at_ms: row
            .try_get("compensation_committed_at_ms")
            .map_err(map_sqlx)?,
    })
}

pub(super) struct NewAuditEvent<'a> {
    pub task_id: &'a str,
    pub sequence: i64,
    pub event_type: AuditEventType,
    pub actor: &'a str,
    pub occurred_at_ms: i64,
    pub payload: &'a AuditPayload,
    pub redacted_error: Option<&'a RedactedError>,
}

pub(super) async fn append_audit(
    tx: &mut Transaction<'_, Sqlite>,
    event: NewAuditEvent<'_>,
) -> McpPlatformResult<()> {
    sqlx::query(
        r#"INSERT INTO audit_events (
            task_id, sequence, event_type, actor, occurred_at_ms, payload_json,
            redacted_error_json
        ) VALUES (?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(event.task_id)
    .bind(event.sequence)
    .bind(event.event_type.as_str())
    .bind(event.actor)
    .bind(event.occurred_at_ms)
    .bind(encode(event.payload)?)
    .bind(encode_optional(event.redacted_error)?)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

pub(super) fn decode_audit_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<AuditEventRecord> {
    Ok(AuditEventRecord {
        event_id: row.try_get("event_id").map_err(map_sqlx)?,
        task_id: row.try_get("task_id").map_err(map_sqlx)?,
        sequence: row.try_get("sequence").map_err(map_sqlx)?,
        event_type: decode_database_enum(
            &row.try_get::<String, _>("event_type").map_err(map_sqlx)?,
        )?,
        actor: row.try_get("actor").map_err(map_sqlx)?,
        occurred_at_ms: row.try_get("occurred_at_ms").map_err(map_sqlx)?,
        payload: decode(&row.try_get::<String, _>("payload_json").map_err(map_sqlx)?)?,
        redacted_error: decode_optional(
            row.try_get::<Option<String>, _>("redacted_error_json")
                .map_err(map_sqlx)?,
        )?,
    })
}

pub(super) fn recovery_decision(task: &TaskRecord, steps: &[TaskStepRecord]) -> RecoveryDecision {
    let adapter_can_resume = task
        .adapter_evidence
        .as_ref()
        .is_some_and(|evidence| evidence.compatible_for_recovery && evidence.resume_safe);
    let rollback_available = task
        .rollback_evidence
        .as_ref()
        .is_some_and(|evidence| evidence.compensation_available);
    let last_committed = steps
        .iter()
        .filter(|step| step.status == TaskStepStatus::Committed)
        .map(|step| step.ordinal)
        .max();
    let started = steps
        .iter()
        .find(|step| step.status == TaskStepStatus::Started)
        .map(|step| step.ordinal);

    if matches!(
        task.status,
        TaskStatus::RollingBack | TaskStatus::Cancelling
    ) {
        return match (rollback_available, last_committed) {
            (true, Some(ordinal)) => RecoveryDecision::RollbackFromStep { ordinal },
            _ => RecoveryDecision::RequiresManualRecovery,
        };
    }
    if adapter_can_resume {
        return RecoveryDecision::ResumeFromStep {
            ordinal: started
                .or_else(|| last_committed.map(|ordinal| ordinal + 1))
                .unwrap_or(0),
        };
    }
    match (rollback_available, last_committed) {
        (true, Some(ordinal)) => RecoveryDecision::RollbackFromStep { ordinal },
        _ => RecoveryDecision::RequiresManualRecovery,
    }
}

pub(super) fn validate_manifest_proof(
    proof: &ManifestProof,
    digest: &str,
) -> McpPlatformResult<()> {
    if let ManifestProof::Catalog {
        declared_manifest_digest,
        ..
    } = proof
    {
        if declared_manifest_digest != digest {
            return Err(integrity_error());
        }
    }
    Ok(())
}

pub(super) fn decode_manifest_source_metadata_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<ManifestSourceMetadata> {
    let source_ref_json = row
        .try_get::<Option<String>, _>("source_ref_json")
        .map_err(map_sqlx)?;
    let import_kind = row
        .try_get::<Option<String>, _>("import_kind")
        .map_err(map_sqlx)?;
    let release_id: Option<String> = row.try_get("release_id").map_err(map_sqlx)?;
    let origin_provenance_json = row
        .try_get::<Option<String>, _>("origin_provenance_json")
        .map_err(map_sqlx)?;
    let update_channel_json = row
        .try_get::<Option<String>, _>("update_channel_json")
        .map_err(map_sqlx)?;
    if source_ref_json.is_none()
        && import_kind.is_none()
        && release_id.is_none()
        && origin_provenance_json.is_none()
        && update_channel_json.is_none()
    {
        return Ok(ManifestSourceMetadata::local_persistence());
    }
    let source_ref = decode(source_ref_json.as_deref().ok_or_else(integrity_error)?)?;
    let import_kind = decode_database_enum::<SourceImportKind>(
        import_kind.as_deref().ok_or_else(integrity_error)?,
    )?;
    Ok(ManifestSourceMetadata {
        source_ref,
        import_kind,
        release_id,
        origin_provenance: decode_optional(origin_provenance_json)?.unwrap_or_default(),
        update_channel: decode_optional(update_channel_json)?.unwrap_or_default(),
    })
}

pub(super) fn encode<T: Serialize + ?Sized>(value: &T) -> McpPlatformResult<String> {
    serde_json::to_string(value).map_err(|_| integrity_error())
}

pub(super) fn encode_optional<T: Serialize>(
    value: Option<&T>,
) -> McpPlatformResult<Option<String>> {
    value.map(encode).transpose()
}

pub(super) fn decode<T: DeserializeOwned>(value: &str) -> McpPlatformResult<T> {
    serde_json::from_str(value).map_err(|_| integrity_error())
}

pub(super) fn decode_optional<T: DeserializeOwned>(
    value: Option<String>,
) -> McpPlatformResult<Option<T>> {
    value.map(|value| decode(&value)).transpose()
}

pub(super) fn decode_database_enum<T: DeserializeOwned>(value: &str) -> McpPlatformResult<T> {
    decode(&serde_json::to_string(value).map_err(|_| integrity_error())?)
}

pub(super) trait IntoRepositoryError {
    fn into_repository_error(self) -> McpPlatformError;
}

impl IntoRepositoryError for sqlx::Error {
    fn into_repository_error(self) -> McpPlatformError {
        repository_unavailable()
    }
}

impl IntoRepositoryError for McpPlatformError {
    fn into_repository_error(self) -> McpPlatformError {
        self
    }
}

pub(super) fn map_sqlx(error: impl IntoRepositoryError) -> McpPlatformError {
    error.into_repository_error()
}

pub(super) const fn repository_unavailable() -> McpPlatformError {
    error(
        McpPlatformErrorCode::RepositoryUnavailable,
        "MCP platform repository operation failed",
    )
}

pub(super) const fn not_found() -> McpPlatformError {
    error(
        McpPlatformErrorCode::NotFound,
        "MCP platform record was not found",
    )
}

pub(super) const fn revision_conflict() -> McpPlatformError {
    error(
        McpPlatformErrorCode::RevisionConflict,
        "record revision no longer matches",
    )
}

pub(super) const fn integrity_error() -> McpPlatformError {
    error(
        McpPlatformErrorCode::IntegrityError,
        "stored MCP platform record failed integrity validation",
    )
}

pub(super) const fn error(code: McpPlatformErrorCode, message: &'static str) -> McpPlatformError {
    McpPlatformError::new(code, message)
}
