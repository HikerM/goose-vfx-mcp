use rand::RngExt;
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction};

use crate::mcp_platform::domain::{InstallationState, ManagedMcpState, RegistrationState};
use crate::mcp_platform::effect_fence::{
    EffectFenceBinding, EffectFenceGuard, EffectFenceRepositoryIdentity,
};
use crate::mcp_platform::error::{McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::fenced_projection_sink::{
    fenced_command_binding, AcceptedFencedReceipt, BoundFencedProjectionEffectGrant,
};
use crate::mcp_platform::projection_runtime::{
    authoritative_persisted_state_digest, expected_observed_state_digest,
    projection_binding_digest, ProjectionWriteCapability, RuntimeProjectionAuthority,
};
use crate::mcp_platform::repository::{
    projection_sink_receipt_binding_payload, AbandonProjectionEffectGrant,
    ActivateManagedInstallation, AuditEventType, AuditPayload, ConnectionProjectionRecord,
    ConsumingProjectionEffectGrant, CreateHealthTask, FinishProjectionEffectGrant,
    HealthObservationRecord, HealthTaskRequestRecord, IssuedProjectionEffectGrant,
    ManagedMcpInventoryRecord, ManagedMcpLifecycleMetadata, ManagedUninstallSnapshot,
    NewHealthObservation, ProjectionAuthorityAnchor, ProjectionAuthorization,
    ProjectionEffectGrantStatus, ProjectionMutationRecord, ProjectionMutationStatus,
    ProjectionRecoveryConfirmationReceipt, ProjectionRepositoryIdentity,
    ProjectionSinkAtomicProofKind, ProjectionSinkCommitReceipt, PutOwnedProjection,
    RegisterManagedMcp, RegisterManagedMcpOutcome, RemoveOwnedProjection, RestoreOwnedProjection,
    RetryAttemptRecord, StageManagedInstallation, StageManagedInstallationOutcome, TaskRecord,
    PROJECTION_SINK_COMMIT_RECEIPT_DOMAIN, PROJECTION_SINK_RECOVERY_RECEIPT_DOMAIN,
};
use crate::mcp_platform::task::{TaskOperation, TaskStatus};
use crate::mcp_platform::SupplyChainEvidence;

use super::records::{
    append_audit, decode, decode_database_enum, decode_managed_row, decode_managed_version_row,
    decode_manifest_row, decode_task_row, encode, error, fetch_task, fetch_task_step,
    fetch_valid_plan, integrity_error, managed_projection_integrity_payload, map_sqlx, not_found,
    projection_writer_payload, NewAuditEvent,
};
use super::SqliteMcpPlatformRepository;

struct OwnedProjectionRestoreValidation {
    projection_json: String,
    already_restored: bool,
    current_revision: i64,
}

impl SqliteMcpPlatformRepository {
    /// Acquires the OS effect gateway and captures the current anchored state.
    ///
    /// The guard intentionally outlives any individual SQLite transaction. V41
    /// does not execute an effect while it is held; V42 must bind it to a sink.
    pub(crate) async fn acquire_projection_effect_fence(
        &self,
    ) -> McpPlatformResult<EffectFenceGuard> {
        let identity = self.effect_fence_repository_identity()?;
        let guard = EffectFenceGuard::acquire(identity)?;
        let mut tx = self.begin_immediate().await?;
        ensure_effect_grant_eligibility(&mut tx).await?;
        let binding = effect_fence_binding(&mut tx, guard.scope()).await?;
        guard.bind(binding)?;
        guard.confirm_abandoned_mutex_revalidated();
        tx.rollback().await?;
        Ok(guard)
    }

    pub(crate) async fn issue_bound_fenced_projection_effect_grant(
        &self,
        guard: &EffectFenceGuard,
        input: BoundFencedProjectionEffectGrant<'_>,
    ) -> McpPlatformResult<IssuedProjectionEffectGrant> {
        validate_effect_identifier(input.effect_id)?;
        validate_effect_digest(input.target_digest)?;
        validate_fenced_sink_binding(&input.sink)?;
        if input.now_ms < 0 {
            return Err(integrity_error());
        }

        let mut tx = self.begin_immediate().await?;
        ensure_effect_grant_eligibility(&mut tx).await?;
        let binding = effect_fence_binding(&mut tx, guard.scope()).await?;
        self.verify_effect_fence_guard(guard, &binding)?;
        ensure_single_fenced_effect_lifecycle(&mut tx, guard.scope(), input.effect_id).await?;
        let mut nonce = [0_u8; 32];
        rand::rng().fill(&mut nonce);
        let grant_id = uuid::Uuid::new_v4().to_string();
        let nonce_hash = Sha256::digest(nonce).to_vec();
        let fence_epoch = binding
            .fence_epoch
            .checked_add(1)
            .ok_or_else(integrity_error)?;
        let canonical_digest = tx.checkpoint.root.clone();
        let identity = self.effect_fence_repository_identity()?;
        let repository_key_epoch =
            i64::try_from(identity.key_epoch).map_err(|_| integrity_error())?;
        let command_binding = fenced_command_binding(
            &identity.instance_id,
            &identity.path_binding,
            repository_key_epoch,
            input.sink.sink_identity(),
            input.sink.sink_version(),
            input.sink.authority_id(),
            i64::try_from(input.sink.authority_key_epoch()).map_err(|_| integrity_error())?,
            input.sink.authority_key_fingerprint(),
            &grant_id,
            input.effect_id,
            fence_epoch,
            input.target_digest,
            &canonical_digest,
        );
        validate_effect_digest(&canonical_digest)?;

        sqlx::query(
            "INSERT INTO effect_fences(scope,epoch,canonical_digest,created_at_ms) VALUES ('global',?,?,?)",
        )
        .bind(fence_epoch)
        .bind(&canonical_digest)
        .bind(input.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "INSERT INTO effect_grants(grant_id,effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,issued_at_ms,transitioned_at_ms) VALUES (?,?,'global',?,?,?,?, 'issued',0,?,?)",
        )
        .bind(&grant_id)
        .bind(input.effect_id)
        .bind(fence_epoch)
        .bind(input.target_digest)
        .bind(&canonical_digest)
        .bind(nonce_hash)
        .bind(input.now_ms)
        .bind(input.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "INSERT INTO effect_fenced_sink_bindings(grant_id,effect_id,sink_identity,sink_version,sink_authority,authority_binding_format,authority_key_epoch,authority_key_fingerprint,command_binding,repository_instance_id,repository_path_binding,repository_key_epoch,fence_epoch,target_digest,canonical_digest) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&grant_id).bind(input.effect_id).bind(input.sink.sink_identity())
        .bind(input.sink.sink_version()).bind(input.sink.authority_id())
        .bind(1_i64)
        .bind(i64::try_from(input.sink.authority_key_epoch()).map_err(|_| integrity_error())?)
        .bind(input.sink.authority_key_fingerprint()).bind(&command_binding)
        .bind(&identity.instance_id).bind(&identity.path_binding).bind(repository_key_epoch)
        .bind(fence_epoch).bind(input.target_digest).bind(&canonical_digest)
        .execute(&mut **tx).await.map_err(map_sqlx)?;
        insert_effect_audit(
            &mut tx,
            &grant_id,
            input.effect_id,
            0,
            ProjectionEffectGrantStatus::Issued,
            input.target_digest,
            &canonical_digest,
            None,
            input.now_ms,
        )
        .await?;
        let checkpoint = tx.commit_with_checkpoint().await?;
        self.advance_effect_fence_guard(guard, fence_epoch, checkpoint)?;

        Ok(IssuedProjectionEffectGrant {
            grant_id,
            effect_id: input.effect_id.to_string(),
            fence_epoch,
            target_digest: input.target_digest.to_string(),
            canonical_digest,
            sink_identity: input.sink.sink_identity().to_string(),
            sink_version: input.sink.sink_version().to_string(),
            sink_authority: input.sink.authority_id().to_string(),
            authority_key_epoch: i64::try_from(input.sink.authority_key_epoch())
                .map_err(|_| integrity_error())?,
            authority_key_fingerprint: input.sink.authority_key_fingerprint().to_string(),
            command_binding,
            repository_instance_id: identity.instance_id,
            repository_path_binding: identity.path_binding,
            repository_key_epoch,
            nonce,
        })
    }

    #[cfg(test)]
    pub(crate) async fn issue_projection_effect_grant(
        &self,
        guard: &EffectFenceGuard,
        input: crate::mcp_platform::repository::IssueProjectionEffectGrant<'_>,
    ) -> McpPlatformResult<IssuedProjectionEffectGrant> {
        self.issue_bound_fenced_projection_effect_grant(
            guard,
            BoundFencedProjectionEffectGrant {
                effect_id: input.effect_id,
                target_digest: input.target_digest,
                sink: super::super::super::fenced_projection_sink::test_sink_issue_binding()?,
                now_ms: input.now_ms,
            },
        )
        .await
    }

    pub(crate) async fn consume_projection_effect_grant(
        &self,
        guard: &EffectFenceGuard,
        grant: &IssuedProjectionEffectGrant,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        transition_projection_effect_grant(
            self,
            guard,
            grant.grant_id(),
            Some(grant),
            None,
            Some(ProjectionEffectGrantStatus::Issued),
            ProjectionEffectGrantStatus::Consuming,
            None,
            now_ms,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn finish_projection_effect_grant(
        &self,
        guard: &EffectFenceGuard,
        input: FinishProjectionEffectGrant<'_>,
    ) -> McpPlatformResult<()> {
        let receipt = self.test_fenced_receipt(input.grant_id).await?;
        transition_projection_effect_grant(
            self,
            guard,
            input.grant_id,
            None,
            Some(&receipt),
            Some(ProjectionEffectGrantStatus::Consuming),
            ProjectionEffectGrantStatus::Applied,
            None,
            input.now_ms,
        )
        .await
    }

    #[cfg(test)]
    async fn test_fenced_receipt(
        &self,
        grant_id: &str,
    ) -> McpPlatformResult<AcceptedFencedReceipt> {
        let mut tx = self.begin_immediate().await?;
        let row = sqlx::query(
            "SELECT grant_id,effect_id,sink_identity,sink_version,sink_authority,authority_key_epoch,authority_key_fingerprint,command_binding,fence_epoch,target_digest FROM effect_fenced_sink_bindings WHERE grant_id=?",
        )
        .bind(grant_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(integrity_error)?;
        let receipt = AcceptedFencedReceipt::for_test(
            row.try_get("grant_id").map_err(map_sqlx)?,
            row.try_get("effect_id").map_err(map_sqlx)?,
            row.try_get("sink_identity").map_err(map_sqlx)?,
            row.try_get("sink_version").map_err(map_sqlx)?,
            row.try_get("sink_authority").map_err(map_sqlx)?,
            u64::try_from(
                row.try_get::<i64, _>("authority_key_epoch")
                    .map_err(map_sqlx)?,
            )
            .map_err(|_| integrity_error())?,
            row.try_get("authority_key_fingerprint").map_err(map_sqlx)?,
            row.try_get("command_binding").map_err(map_sqlx)?,
            row.try_get("fence_epoch").map_err(map_sqlx)?,
            row.try_get("target_digest").map_err(map_sqlx)?,
        );
        tx.rollback().await?;
        Ok(receipt)
    }

    pub(crate) async fn finish_projection_effect_grant_with_conditional_receipt(
        &self,
        guard: &EffectFenceGuard,
        input: FinishProjectionEffectGrant<'_>,
        receipt: AcceptedFencedReceipt,
    ) -> McpPlatformResult<()> {
        transition_projection_effect_grant(
            self,
            guard,
            input.grant_id,
            None,
            Some(&receipt),
            Some(ProjectionEffectGrantStatus::Consuming),
            ProjectionEffectGrantStatus::Applied,
            None,
            input.now_ms,
        )
        .await
    }

    pub(crate) async fn mark_projection_effect_grant_unknown(
        &self,
        guard: &EffectFenceGuard,
        input: FinishProjectionEffectGrant<'_>,
    ) -> McpPlatformResult<()> {
        transition_projection_effect_grant(
            self,
            guard,
            input.grant_id,
            None,
            None,
            Some(ProjectionEffectGrantStatus::Consuming),
            ProjectionEffectGrantStatus::Unknown,
            None,
            input.now_ms,
        )
        .await
    }

    pub(crate) async fn abandon_projection_effect_grant(
        &self,
        guard: &EffectFenceGuard,
        input: AbandonProjectionEffectGrant<'_>,
    ) -> McpPlatformResult<()> {
        transition_projection_effect_grant(
            self,
            guard,
            input.grant_id,
            None,
            None,
            None,
            ProjectionEffectGrantStatus::Abandoned,
            Some(input.reason),
            input.now_ms,
        )
        .await
    }

    /// Loads a consumed grant for receipt-only reconciliation. This never
    /// authorizes another external apply; callers must use sink receipt lookup.
    pub(crate) async fn load_consuming_projection_effect_grant(
        &self,
        effect_id: &str,
    ) -> McpPlatformResult<ConsumingProjectionEffectGrant> {
        validate_effect_identifier(effect_id)?;
        let mut tx = self.begin_immediate().await?;
        ensure_effect_grant_eligibility(&mut tx).await?;
        let row = sqlx::query(
            "SELECT g.grant_id,g.effect_id,g.fence_scope,g.fence_epoch,g.target_digest,g.canonical_digest,g.status,b.sink_identity,b.sink_version,b.sink_authority,b.authority_key_epoch,b.authority_key_fingerprint,b.command_binding,b.repository_instance_id,b.repository_path_binding,b.repository_key_epoch FROM effect_grants g JOIN effect_fenced_sink_bindings b ON b.grant_id=g.grant_id AND b.effect_id=g.effect_id AND b.fence_epoch=g.fence_epoch AND b.target_digest=g.target_digest AND b.canonical_digest=g.canonical_digest WHERE g.effect_id=?",
        )
        .bind(effect_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        if row.try_get::<String, _>("status").map_err(map_sqlx)?
            != ProjectionEffectGrantStatus::Consuming.as_str()
        {
            return Err(integrity_error());
        }
        let grant = ConsumingProjectionEffectGrant {
            grant_id: row.try_get("grant_id").map_err(map_sqlx)?,
            effect_id: row.try_get("effect_id").map_err(map_sqlx)?,
            fence_scope: row.try_get("fence_scope").map_err(map_sqlx)?,
            fence_epoch: row.try_get("fence_epoch").map_err(map_sqlx)?,
            target_digest: row.try_get("target_digest").map_err(map_sqlx)?,
            canonical_digest: row.try_get("canonical_digest").map_err(map_sqlx)?,
            sink_identity: row.try_get("sink_identity").map_err(map_sqlx)?,
            sink_version: row.try_get("sink_version").map_err(map_sqlx)?,
            sink_authority: row.try_get("sink_authority").map_err(map_sqlx)?,
            authority_key_epoch: row.try_get("authority_key_epoch").map_err(map_sqlx)?,
            authority_key_fingerprint: row
                .try_get("authority_key_fingerprint")
                .map_err(map_sqlx)?,
            command_binding: row.try_get("command_binding").map_err(map_sqlx)?,
            repository_instance_id: row.try_get("repository_instance_id").map_err(map_sqlx)?,
            repository_path_binding: row.try_get("repository_path_binding").map_err(map_sqlx)?,
            repository_key_epoch: row.try_get("repository_key_epoch").map_err(map_sqlx)?,
        };
        tx.rollback().await?;
        Ok(grant)
    }
}

async fn transition_projection_effect_grant(
    repository: &SqliteMcpPlatformRepository,
    guard: &EffectFenceGuard,
    grant_id: &str,
    issued_grant: Option<&IssuedProjectionEffectGrant>,
    accepted_receipt: Option<&AcceptedFencedReceipt>,
    expected: Option<ProjectionEffectGrantStatus>,
    next: ProjectionEffectGrantStatus,
    abandon_reason: Option<&str>,
    now_ms: i64,
) -> McpPlatformResult<()> {
    validate_effect_identifier(grant_id)?;
    if now_ms < 0
        || (next == ProjectionEffectGrantStatus::Abandoned && !valid_abandon_reason(abandon_reason))
    {
        return Err(integrity_error());
    }
    let mut tx = repository.begin_immediate().await?;
    ensure_effect_grant_eligibility(&mut tx).await?;
    let binding = effect_fence_binding(&mut tx, guard.scope()).await?;
    repository.verify_effect_fence_guard(guard, &binding)?;
    let row = sqlx::query(
        "SELECT effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,transitioned_at_ms FROM effect_grants WHERE grant_id=?",
    )
    .bind(grant_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx)?
    .ok_or_else(not_found)?;
    let status: String = row.try_get("status").map_err(map_sqlx)?;
    if expected.is_some_and(|expected| status != expected.as_str())
        || (expected.is_none() && !matches!(status.as_str(), "issued" | "consuming"))
    {
        return Err(integrity_error());
    }
    let previous_transitioned_at_ms: i64 = row.try_get("transitioned_at_ms").map_err(map_sqlx)?;
    if now_ms < previous_transitioned_at_ms {
        return Err(integrity_error());
    }
    let fence_scope: String = row.try_get("fence_scope").map_err(map_sqlx)?;
    let fence_epoch: i64 = row.try_get("fence_epoch").map_err(map_sqlx)?;
    if fence_scope != guard.scope() || fence_epoch != binding.fence_epoch {
        return Err(integrity_error());
    }
    if let Some(grant) = issued_grant {
        if grant.grant_id != grant_id
            || grant.fence_epoch != fence_epoch
            || grant.canonical_digest
                != row
                    .try_get::<String, _>("canonical_digest")
                    .map_err(map_sqlx)?
            || grant.effect_id != row.try_get::<String, _>("effect_id").map_err(map_sqlx)?
        {
            return Err(integrity_error());
        }
        let stored_hash: Vec<u8> = row.try_get("nonce_hash").map_err(map_sqlx)?;
        let expected_hash = Sha256::digest(grant.nonce);
        if stored_hash.as_slice() != expected_hash.as_slice() {
            return Err(integrity_error());
        }
    }
    if next == ProjectionEffectGrantStatus::Applied && accepted_receipt.is_none() && !cfg!(test) {
        return Err(integrity_error());
    }
    if let Some(receipt) = accepted_receipt {
        if receipt.grant_id() != grant_id {
            return Err(integrity_error());
        }
        let binding = sqlx::query(
            "SELECT effect_id,sink_identity,sink_version,sink_authority,authority_key_epoch,authority_key_fingerprint,command_binding,fence_epoch,target_digest FROM effect_fenced_sink_bindings WHERE grant_id=?",
        )
        .bind(grant_id)
        .fetch_optional(&mut **tx).await.map_err(map_sqlx)?.ok_or_else(integrity_error)?;
        if binding
            .try_get::<String, _>("effect_id")
            .map_err(map_sqlx)?
            != receipt.effect_id()
            || binding
                .try_get::<String, _>("sink_identity")
                .map_err(map_sqlx)?
                != receipt.sink_identity()
            || binding
                .try_get::<String, _>("sink_version")
                .map_err(map_sqlx)?
                != receipt.sink_version()
            || binding
                .try_get::<String, _>("sink_authority")
                .map_err(map_sqlx)?
                != receipt.sink_authority()
            || binding
                .try_get::<i64, _>("authority_key_epoch")
                .map_err(map_sqlx)?
                != i64::try_from(receipt.authority_key_epoch()).map_err(|_| integrity_error())?
            || binding
                .try_get::<String, _>("authority_key_fingerprint")
                .map_err(map_sqlx)?
                != receipt.authority_key_fingerprint()
            || binding
                .try_get::<String, _>("command_binding")
                .map_err(map_sqlx)?
                != receipt.command_binding()
            || binding.try_get::<i64, _>("fence_epoch").map_err(map_sqlx)? != receipt.fence_epoch()
            || binding
                .try_get::<String, _>("target_digest")
                .map_err(map_sqlx)?
                != receipt.target_digest()
        {
            return Err(integrity_error());
        }
    } else if next != ProjectionEffectGrantStatus::Applied && accepted_receipt.is_some() {
        return Err(integrity_error());
    }
    let effect_id: String = row.try_get("effect_id").map_err(map_sqlx)?;
    let target_digest: String = row.try_get("target_digest").map_err(map_sqlx)?;
    let canonical_digest: String = row.try_get("canonical_digest").map_err(map_sqlx)?;
    let next_state_epoch = row
        .try_get::<i64, _>("state_epoch")
        .map_err(map_sqlx)?
        .checked_add(1)
        .ok_or_else(integrity_error)?;
    insert_effect_audit(
        &mut tx,
        grant_id,
        &effect_id,
        next_state_epoch,
        next,
        &target_digest,
        &canonical_digest,
        abandon_reason,
        now_ms,
    )
    .await?;
    let updated = sqlx::query(
        "UPDATE effect_grants SET status=?,state_epoch=?,transitioned_at_ms=?,abandon_reason=? WHERE grant_id=? AND status=? AND state_epoch=?",
    )
    .bind(next.as_str())
    .bind(next_state_epoch)
    .bind(now_ms)
    .bind(abandon_reason)
    .bind(grant_id)
    .bind(&status)
    .bind(next_state_epoch - 1)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    if updated.rows_affected() != 1 {
        return Err(integrity_error());
    }
    if let Some(receipt) = accepted_receipt {
        sqlx::query(
            "INSERT INTO effect_fenced_receipts(receipt_id,grant_id,effect_id,sink_identity,sink_version,sink_authority,authority_binding_format,authority_key_epoch,authority_key_fingerprint,command_binding,receipt_binding,outcome,fence_epoch,target_digest) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(receipt.receipt_id())
        .bind(receipt.grant_id())
        .bind(receipt.effect_id())
        .bind(receipt.sink_identity())
        .bind(receipt.sink_version())
        .bind(receipt.sink_authority())
        .bind(1_i64)
        .bind(i64::try_from(receipt.authority_key_epoch()).map_err(|_| integrity_error())?)
        .bind(receipt.authority_key_fingerprint())
        .bind(receipt.command_binding())
        .bind(receipt.receipt_binding())
        .bind("applied")
        .bind(receipt.fence_epoch())
        .bind(receipt.target_digest())
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    }
    let checkpoint = tx.commit_with_checkpoint().await?;
    repository.advance_effect_fence_guard(guard, fence_epoch, checkpoint)
}

impl SqliteMcpPlatformRepository {
    fn effect_fence_repository_identity(&self) -> McpPlatformResult<EffectFenceRepositoryIdentity> {
        let identity = self.integrity_signer.identity()?;
        Ok(EffectFenceRepositoryIdentity {
            instance_id: identity.instance_id,
            path_binding: identity.path_binding,
            key_epoch: identity.key_epoch,
        })
    }

    fn verify_effect_fence_guard(
        &self,
        guard: &EffectFenceGuard,
        binding: &EffectFenceBinding,
    ) -> McpPlatformResult<()> {
        guard.verify(&self.effect_fence_repository_identity()?, binding)
    }

    fn advance_effect_fence_guard(
        &self,
        guard: &EffectFenceGuard,
        fence_epoch: i64,
        checkpoint: super::integrity::AnchorCheckpoint,
    ) -> McpPlatformResult<()> {
        guard.advance(
            &self.effect_fence_repository_identity()?,
            EffectFenceBinding {
                anchor_sequence: checkpoint.sequence,
                anchor_root: checkpoint.root,
                fence_epoch,
            },
        )
    }
}

async fn effect_fence_binding(
    tx: &mut super::AnchoredTransaction<'_>,
    scope: &str,
) -> McpPlatformResult<EffectFenceBinding> {
    if scope != "global" {
        return Err(integrity_error());
    }
    let fence_epoch = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MAX(epoch) FROM effect_fences WHERE scope='global'",
    )
    .fetch_one(&mut ***tx)
    .await
    .map_err(map_sqlx)?
    .unwrap_or(0);
    if fence_epoch < 0 {
        return Err(integrity_error());
    }
    Ok(EffectFenceBinding {
        anchor_sequence: tx.checkpoint.sequence,
        anchor_root: tx.checkpoint.root.clone(),
        fence_epoch,
    })
}

async fn ensure_effect_grant_eligibility(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<()> {
    super::migrations::validate_governed_source_refresh_schema(tx).await?;
    if !matches!(
        super::recovery_eligibility_in_transaction(tx).await?,
        crate::mcp_platform::repository::RecoveryEligibility::Eligible
    ) {
        return Err(integrity_error());
    }
    Ok(())
}

async fn ensure_single_fenced_effect_lifecycle(
    tx: &mut Transaction<'_, Sqlite>,
    scope: &str,
    effect_id: &str,
) -> McpPlatformResult<()> {
    if scope != "global" {
        return Err(integrity_error());
    }
    let active_grant_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM effect_grants WHERE fence_scope=? AND status IN ('issued','consuming')",
    )
    .bind(scope)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    let matching_effect_count =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM effect_grants WHERE effect_id=?")
            .bind(effect_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    if active_grant_count != 0 || matching_effect_count != 0 {
        return Err(integrity_error());
    }
    Ok(())
}

async fn insert_effect_audit(
    tx: &mut Transaction<'_, Sqlite>,
    grant_id: &str,
    effect_id: &str,
    state_epoch: i64,
    event_kind: ProjectionEffectGrantStatus,
    target_digest: &str,
    canonical_digest: &str,
    abandon_reason: Option<&str>,
    occurred_at_ms: i64,
) -> McpPlatformResult<()> {
    sqlx::query(
        "INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms,abandon_reason) VALUES (?,?,?,?,?,?,?,?,?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(grant_id)
    .bind(effect_id)
    .bind(state_epoch)
    .bind(event_kind.as_str())
    .bind(target_digest)
    .bind(canonical_digest)
    .bind(occurred_at_ms)
    .bind(abandon_reason)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

fn validate_fenced_sink_binding(
    binding: &crate::mcp_platform::fenced_projection_sink::FencedSinkIssueBinding,
) -> McpPlatformResult<()> {
    let valid_text = |value: &str| !value.is_empty() && value.len() <= 128 && !value.contains('\0');
    if !valid_text(binding.sink_identity())
        || !valid_text(binding.sink_version())
        || !valid_text(binding.authority_id())
        || binding.authority_key_epoch() == 0
        || binding.authority_key_fingerprint().len() != 64
        || !binding
            .authority_key_fingerprint()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(integrity_error());
    }
    Ok(())
}

fn valid_abandon_reason(value: Option<&str>) -> bool {
    value.is_some_and(|reason| (1..=256).contains(&reason.len()))
}

fn validate_effect_identifier(value: &str) -> McpPlatformResult<()> {
    let parsed = uuid::Uuid::parse_str(value).map_err(|_| integrity_error())?;
    if parsed.hyphenated().to_string() != value {
        return Err(integrity_error());
    }
    Ok(())
}

fn validate_effect_digest(value: &str) -> McpPlatformResult<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(integrity_error());
    }
    Ok(())
}

async fn validate_owned_projection_restore(
    tx: &mut Transaction<'_, Sqlite>,
    signer: &dyn super::integrity::IntegritySigner,
    input: &RestoreOwnedProjection<'_>,
) -> McpPlatformResult<OwnedProjectionRestoreValidation> {
    let projection_json = crate::mcp_platform::encode_projection_config(input.projection)?;
    let decoded = crate::mcp_platform::decode_verified_projection_config(
        &projection_json,
        input.projection_digest,
    )?;
    if &decoded != input.projection {
        return Err(integrity_error());
    }

    let plan = fetch_valid_plan(tx, input.plan_id).await?;
    let scope = plan.target.installation_scope.as_deref().unwrap_or("user");
    let expected_managed_mcp_id =
        crate::mcp_platform::task_runner::stable_managed_mcp_id(plan.plan.manifest_id(), scope);
    let expected_link_key = format!(
        "managed_mcp_{}",
        expected_managed_mcp_id
            .strip_prefix("managed_")
            .ok_or_else(integrity_error)?
    );
    if plan.plan.manifest_digest() != input.manifest_digest
        || plan.plan.connection_projection() != input.projection
        || plan.target.mcp_id != plan.plan.manifest_id()
        || plan
            .target
            .managed_mcp_id
            .as_deref()
            .is_some_and(|id| id != input.managed_mcp_id)
        || expected_managed_mcp_id != input.managed_mcp_id
        || expected_link_key != input.link_key
    {
        return Err(integrity_error());
    }

    let owner_task_id = input.owner_task_id.ok_or_else(integrity_error)?;
    let owner_task = fetch_task(tx, owner_task_id).await?;
    if owner_task.plan_id != input.plan_id
        || owner_task.plan_digest != plan.plan.plan_digest()
        || owner_task.operation != plan.plan.operation().into()
    {
        return Err(integrity_error());
    }
    verify_projection_writer_binding(
        tx,
        signer,
        input.managed_mcp_id,
        owner_task_id,
        input.plan_id,
        input.link_key,
        input.manifest_digest,
        input.projection_digest,
    )
    .await?;

    let replacing_task = fetch_task(tx, input.replacing_task_id).await?;
    if replacing_task.status != TaskStatus::RollingBack
        || replacing_task.owner_id.as_deref() != Some(input.worker_owner_id)
        || replacing_task
            .lease_expires_at_ms
            .is_none_or(|expires_at_ms| expires_at_ms <= input.now_ms)
    {
        return Err(integrity_error());
    }
    let lifecycle_owner = sqlx::query_scalar::<_, String>(
        "SELECT task_id FROM managed_lifecycle_leases WHERE managed_mcp_id=? AND expires_at_ms>?",
    )
    .bind(input.managed_mcp_id)
    .bind(input.now_ms)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx)?
    .ok_or_else(integrity_error)?;
    if lifecycle_owner != input.replacing_task_id {
        return Err(integrity_error());
    }
    let compensation_step = fetch_task_step(
        tx,
        signer,
        input.replacing_task_id,
        input.compensation_ordinal,
    )
    .await?;
    let compensation_matches = matches!(
        &compensation_step.compensation,
        crate::mcp_platform::task::CompensationDescriptor::RestoreManagedProjection {
            managed_mcp_id,
            link_key,
            projection,
            projection_digest,
            manifest_digest,
            plan_id,
            owner_task_id,
            ..
        } if managed_mcp_id == input.managed_mcp_id
            && link_key == input.link_key
            && projection == input.projection
            && projection_digest == input.projection_digest
            && manifest_digest == input.manifest_digest
            && plan_id == input.plan_id
            && owner_task_id.as_deref() == input.owner_task_id
    );
    if !compensation_matches
        || compensation_step.compensation_status
            != crate::mcp_platform::task::CompensationStatus::Started
    {
        return Err(integrity_error());
    }
    let replacing_plan = fetch_valid_plan(tx, &replacing_task.plan_id).await?;
    let replacing_scope = replacing_plan
        .target
        .installation_scope
        .as_deref()
        .unwrap_or("user");
    if replacing_task.plan_digest != replacing_plan.plan.plan_digest()
        || replacing_task.operation != replacing_plan.plan.operation().into()
        || crate::mcp_platform::task_runner::stable_managed_mcp_id(
            replacing_plan.plan.manifest_id(),
            replacing_scope,
        ) != input.managed_mcp_id
    {
        return Err(integrity_error());
    }
    verify_projection_writer_binding(
        tx,
        signer,
        input.managed_mcp_id,
        input.replacing_task_id,
        &replacing_task.plan_id,
        input.link_key,
        replacing_plan.plan.manifest_digest(),
        &crate::mcp_platform::projection_config_digest(
            replacing_plan.plan.connection_projection(),
        )?,
    )
    .await?;

    let current = sqlx::query(
        r#"SELECT projection_json,projection_digest,revision,plan_id,manifest_digest,owner_task_id
           FROM connection_projections WHERE managed_mcp_id = ? AND link_key = ?"#,
    )
    .bind(input.managed_mcp_id)
    .bind(input.link_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx)?
    .ok_or_else(not_found)?;
    let current_digest: String = current.try_get("projection_digest").map_err(map_sqlx)?;
    let current_projection = crate::mcp_platform::decode_verified_projection_config(
        &current
            .try_get::<String, _>("projection_json")
            .map_err(map_sqlx)?,
        &current_digest,
    )?;
    let current_plan_id: String = current.try_get("plan_id").map_err(map_sqlx)?;
    let current_manifest_digest: String = current.try_get("manifest_digest").map_err(map_sqlx)?;
    let current_owner_task_id: Option<String> =
        current.try_get("owner_task_id").map_err(map_sqlx)?;
    let current_revision: i64 = current.try_get("revision").map_err(map_sqlx)?;
    let current_mac = sqlx::query_scalar::<_, String>(
        "SELECT mac FROM managed_projection_integrity WHERE managed_mcp_id=?",
    )
    .bind(input.managed_mcp_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx)?
    .ok_or_else(integrity_error)?;
    signer.verify(
        "managed-projection",
        &managed_projection_integrity_payload(
            input.managed_mcp_id,
            input.link_key,
            &current_digest,
            current_revision,
            Some(&current_plan_id),
            Some(&current_manifest_digest),
            current_owner_task_id.as_deref(),
        ),
        &current_mac,
    )?;
    let already_restored = current_digest == input.projection_digest
        && &current_projection == input.projection
        && current_plan_id == input.plan_id
        && current_manifest_digest == input.manifest_digest
        && current_owner_task_id.as_deref() == Some(owner_task_id);
    let replacing_state_valid = if already_restored {
        matches!(
            replacing_task.status,
            TaskStatus::RollingBack
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Interrupted
                | TaskStatus::RecoveryRequired
        )
    } else {
        replacing_task.status == TaskStatus::RollingBack
    };
    if !replacing_state_valid {
        return Err(integrity_error());
    }
    if !already_restored
        && (current_owner_task_id.as_deref() != Some(input.replacing_task_id)
            || current_plan_id != replacing_task.plan_id
            || current_manifest_digest != replacing_plan.plan.manifest_digest()
            || &current_projection != replacing_plan.plan.connection_projection())
    {
        return Err(integrity_error());
    }

    Ok(OwnedProjectionRestoreValidation {
        projection_json,
        already_restored,
        current_revision,
    })
}

pub(super) async fn verify_projection_writer_binding(
    tx: &mut Transaction<'_, Sqlite>,
    signer: &dyn super::integrity::IntegritySigner,
    managed_mcp_id: &str,
    task_id: &str,
    plan_id: &str,
    link_key: &str,
    manifest_digest: &str,
    projection_digest: &str,
) -> McpPlatformResult<()> {
    let row = sqlx::query(
        r#"SELECT plan_digest,lifecycle_acquired_at_ms,worker_owner_id,
                  worker_lease_expires_at_ms,step_ordinal,step_token,mac
           FROM projection_writer_bindings
           WHERE managed_mcp_id=? AND task_id=? AND plan_id=? AND link_key=?
             AND manifest_digest=? AND projection_digest=?"#,
    )
    .bind(managed_mcp_id)
    .bind(task_id)
    .bind(plan_id)
    .bind(link_key)
    .bind(manifest_digest)
    .bind(projection_digest)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx)?
    .ok_or_else(integrity_error)?;
    let plan_digest: String = row.try_get("plan_digest").map_err(map_sqlx)?;
    let lifecycle_acquired_at_ms: i64 =
        row.try_get("lifecycle_acquired_at_ms").map_err(map_sqlx)?;
    let worker_owner_id: String = row.try_get("worker_owner_id").map_err(map_sqlx)?;
    let worker_lease_expires_at_ms: i64 = row
        .try_get("worker_lease_expires_at_ms")
        .map_err(map_sqlx)?;
    let step_ordinal: i64 = row.try_get("step_ordinal").map_err(map_sqlx)?;
    let step_token: String = row.try_get("step_token").map_err(map_sqlx)?;
    let mac: String = row.try_get("mac").map_err(map_sqlx)?;
    signer.verify(
        "projection-writer-authorization",
        &projection_writer_payload(
            managed_mcp_id,
            task_id,
            plan_id,
            &plan_digest,
            link_key,
            manifest_digest,
            projection_digest,
            lifecycle_acquired_at_ms,
            &worker_owner_id,
            worker_lease_expires_at_ms,
            step_ordinal,
            &step_token,
        ),
        &mac,
    )
}

impl SqliteMcpPlatformRepository {
    pub(crate) async fn verified_projection_writer_repository_identity(
        &self,
    ) -> McpPlatformResult<ProjectionRepositoryIdentity> {
        let tx = self.begin_immediate().await?;
        let identity = ProjectionRepositoryIdentity {
            provider_id: tx.checkpoint.identity.provider_id.clone(),
            instance_id: tx.checkpoint.identity.instance_id.clone(),
            path_binding: tx.checkpoint.identity.path_binding.clone(),
            key_epoch: tx.checkpoint.identity.key_epoch,
        };
        tx.rollback().await?;
        if identity.provider_id.is_empty() {
            return Err(integrity_error());
        }
        Ok(identity)
    }

    pub(super) async fn fetch_connection_projection_snapshot(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<ConnectionProjectionRecord>> {
        let Some(row) = sqlx::query(
            r#"SELECT managed_mcp_id,link_key,projection_json,revision,updated_at_ms,
                      plan_id,manifest_digest,owner_task_id,projection_digest
               FROM connection_projections WHERE managed_mcp_id=?"#,
        )
        .bind(managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        else {
            return Ok(None);
        };
        let projection_json: String = row.try_get("projection_json").map_err(map_sqlx)?;
        let projection_digest: String = row.try_get("projection_digest").map_err(map_sqlx)?;
        if projection_digest.is_empty() {
            return Err(integrity_error());
        }
        let record = ConnectionProjectionRecord {
            managed_mcp_id: row.try_get("managed_mcp_id").map_err(map_sqlx)?,
            link_key: row.try_get("link_key").map_err(map_sqlx)?,
            projection: crate::mcp_platform::decode_verified_projection_config(
                &projection_json,
                &projection_digest,
            )?,
            revision: row.try_get("revision").map_err(map_sqlx)?,
            updated_at_ms: row.try_get("updated_at_ms").map_err(map_sqlx)?,
            plan_id: row.try_get("plan_id").map_err(map_sqlx)?,
            manifest_digest: row.try_get("manifest_digest").map_err(map_sqlx)?,
            owner_task_id: row.try_get("owner_task_id").map_err(map_sqlx)?,
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
        let (Some(plan_id), Some(manifest_digest), Some(owner_task_id)) = (
            record.plan_id.as_deref(),
            record.manifest_digest.as_deref(),
            record.owner_task_id.as_deref(),
        ) else {
            return Err(integrity_error());
        };
        let plan = fetch_valid_plan(tx, plan_id).await?;
        let owner_task = fetch_task(tx, owner_task_id).await?;
        let scope = plan.target.installation_scope.as_deref().unwrap_or("user");
        let expected_managed_mcp_id =
            crate::mcp_platform::task_runner::stable_managed_mcp_id(plan.plan.manifest_id(), scope);
        let expected_link_key = format!(
            "managed_mcp_{}",
            expected_managed_mcp_id
                .strip_prefix("managed_")
                .ok_or_else(integrity_error)?
        );
        if plan.plan.manifest_digest() != manifest_digest
            || plan.target.mcp_id != plan.plan.manifest_id()
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
        verify_projection_writer_binding(
            tx,
            self.integrity_signer.as_ref(),
            managed_mcp_id,
            owner_task_id,
            plan_id,
            &record.link_key,
            manifest_digest,
            &record.projection_digest,
        )
        .await?;
        Ok(Some(record))
    }

    pub(crate) async fn claim_artifact(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
        stale_before_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let row = sqlx::query("SELECT owner_task_id,status,updated_at_ms FROM artifact_claims WHERE artifact_digest = ?")
            .bind(artifact_digest).fetch_optional(&mut **tx).await.map_err(map_sqlx)?;
        match row {
            None => {
                sqlx::query("INSERT INTO artifact_claims(artifact_digest,owner_task_id,status,updated_at_ms) VALUES (?,?,'fetching',?)")
                    .bind(artifact_digest).bind(task_id).bind(now_ms).execute(&mut **tx).await.map_err(map_sqlx)?;
            }
            Some(row) => {
                let owner: String = row.try_get("owner_task_id").map_err(map_sqlx)?;
                let status: String = row.try_get("status").map_err(map_sqlx)?;
                let updated: i64 = row.try_get("updated_at_ms").map_err(map_sqlx)?;
                if owner != task_id && status != "released" && updated >= stale_before_ms {
                    return Err(crate::mcp_platform::error::McpPlatformError::new(
                        McpPlatformErrorCode::PlanConflict,
                        "managed artifact is claimed by another lifecycle task",
                    ));
                }
                sqlx::query("UPDATE artifact_claims SET owner_task_id = ?,status = 'fetching',updated_at_ms = ? WHERE artifact_digest = ?")
                    .bind(task_id).bind(now_ms).bind(artifact_digest).execute(&mut **tx).await.map_err(map_sqlx)?;
            }
        }
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn mark_artifact_claim_verified(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let result = sqlx::query("UPDATE artifact_claims SET status = 'verified',updated_at_ms = ? WHERE artifact_digest = ? AND owner_task_id = ? AND status IN ('fetching','verified')")
            .bind(now_ms).bind(artifact_digest).bind(task_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            return Err(integrity_error());
        }
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn release_artifact_claim(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let result = sqlx::query("UPDATE artifact_claims SET status = 'released',updated_at_ms = ? WHERE artifact_digest = ? AND owner_task_id = ? AND status IN ('fetching','verified','released')")
            .bind(now_ms).bind(artifact_digest).bind(task_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            return Err(integrity_error());
        }
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn mark_managed_runtime_activated(
        &self,
        managed_mcp_id: &str,
        target_version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let result = sqlx::query("UPDATE activation_journal SET status = 'pointer_committed',updated_at_ms = ? WHERE managed_mcp_id = ? AND task_id = ? AND target_version = ? AND status IN ('started','pointer_committed')")
            .bind(now_ms).bind(managed_mcp_id).bind(task_id).bind(target_version).execute(&mut **tx).await.map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            return Err(integrity_error());
        }
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn finalize_retained_version_cleanup(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM activation_journal WHERE managed_mcp_id = ? AND task_id = ?",
        )
        .bind(managed_mcp_id)
        .bind(task_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if status == "cleanup_committed" {
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(());
        }
        if status != "health_committed" {
            return Err(integrity_error());
        }
        let row = sqlx::query("SELECT artifact_digest,active,activation_state FROM managed_versions WHERE managed_mcp_id = ? AND version = ?")
            .bind(managed_mcp_id).bind(version).fetch_optional(&mut **tx).await.map_err(map_sqlx)?;
        let Some(row) = row else {
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(());
        };
        if row.try_get::<bool, _>("active").map_err(map_sqlx)?
            || row
                .try_get::<String, _>("activation_state")
                .map_err(map_sqlx)?
                != "retained"
        {
            return Err(integrity_error());
        }
        let owned = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM installation_ownership WHERE managed_mcp_id = ? AND version = ? AND remove_on_uninstall = 1)")
            .bind(managed_mcp_id).bind(version).fetch_one(&mut **tx).await.map_err(map_sqlx)?;
        if !owned {
            return Err(integrity_error());
        }
        let digest: Option<String> = row.try_get("artifact_digest").map_err(map_sqlx)?;
        sqlx::query("DELETE FROM installation_ownership WHERE managed_mcp_id = ? AND version = ?")
            .bind(managed_mcp_id)
            .bind(version)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        sqlx::query(
            "DELETE FROM managed_versions WHERE managed_mcp_id = ? AND version = ? AND active = 0",
        )
        .bind(managed_mcp_id)
        .bind(version)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if let Some(digest) = digest {
            sqlx::query("UPDATE artifact_cache SET reference_count = reference_count - 1 WHERE artifact_digest = ? AND reference_count > 0").bind(digest).execute(&mut **tx).await.map_err(map_sqlx)?;
        }
        sqlx::query("UPDATE activation_journal SET status = 'cleanup_committed',updated_at_ms = ? WHERE managed_mcp_id = ? AND task_id = ? AND status = 'health_committed'")
            .bind(now_ms).bind(managed_mcp_id).bind(task_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    pub async fn is_managed_finalizing(
        &self,
        task_id: &str,
        operation: TaskOperation,
    ) -> McpPlatformResult<bool> {
        match operation {
            TaskOperation::Uninstall => Ok(sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM uninstall_journal WHERE task_id = ? AND status = 'committed')").bind(task_id).fetch_one(&self.pool).await.map_err(map_sqlx)?),
            TaskOperation::Update => Ok(sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM activation_journal WHERE task_id = ? AND status = 'cleanup_committed')").bind(task_id).fetch_one(&self.pool).await.map_err(map_sqlx)?),
            TaskOperation::Repair => Ok(sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM activation_journal a JOIN task_steps s ON s.task_id = a.task_id WHERE a.task_id = ? AND a.status = 'health_committed' AND s.ordinal >= 10 AND s.status = 'started')").bind(task_id).fetch_one(&self.pool).await.map_err(map_sqlx)?),
            _ => Ok(false),
        }
    }

    pub(crate) async fn record_managed_finalization_failure(
        &self,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<i64> {
        let mut tx = self.begin_immediate().await?;
        sqlx::query(
            "UPDATE tasks SET finalization_failures = finalization_failures + 1, updated_at_ms = ? WHERE task_id = ?",
        )
        .bind(now_ms)
        .bind(task_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let failures = sqlx::query_scalar::<_, i64>(
            "SELECT finalization_failures FROM tasks WHERE task_id = ?",
        )
        .bind(task_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(failures)
    }

    pub(crate) async fn stage_managed_installation(
        &self,
        input: StageManagedInstallation<'_>,
    ) -> McpPlatformResult<StageManagedInstallationOutcome> {
        let mut tx = self.begin_immediate().await?;
        let lease_owner = sqlx::query_scalar::<_, String>(
            "SELECT task_id FROM managed_lifecycle_leases WHERE managed_mcp_id = ? AND expires_at_ms > ?",
        )
        .bind(input.managed_mcp_id)
        .bind(input.now_ms)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if lease_owner.as_deref() != Some(input.task_id) {
            return Err(crate::mcp_platform::error::McpPlatformError::new(
                McpPlatformErrorCode::PlanConflict,
                "managed MCP lifecycle lease is not owned by this task",
            ));
        }
        let manifest =
            sqlx::query("SELECT mcp_id, version FROM manifest_blobs WHERE manifest_digest = ?")
                .bind(input.manifest_digest)
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx)?
                .ok_or_else(not_found)?;
        if manifest.try_get::<String, _>("mcp_id").map_err(map_sqlx)? != input.mcp_id
            || manifest.try_get::<String, _>("version").map_err(map_sqlx)? != input.version
        {
            return Err(integrity_error());
        }
        if let Some(journal) = sqlx::query("SELECT previous_version,target_version FROM activation_journal WHERE managed_mcp_id = ? AND task_id = ?")
            .bind(input.managed_mcp_id).bind(input.task_id).fetch_optional(&mut **tx).await.map_err(map_sqlx)?
        {
            let previous_version: Option<String> = journal.try_get("previous_version").map_err(map_sqlx)?;
            if journal.try_get::<String, _>("target_version").map_err(map_sqlx)? != input.version { return Err(integrity_error()); }
            let managed = sqlx::query("SELECT mcp_id,installation_scope,distribution_adapter,owner_task_id FROM managed_mcps WHERE managed_mcp_id = ?")
                .bind(input.managed_mcp_id).fetch_one(&mut **tx).await.map_err(map_sqlx)?;
            if managed.try_get::<String, _>("mcp_id").map_err(map_sqlx)? != input.mcp_id
                || managed.try_get::<String, _>("installation_scope").map_err(map_sqlx)? != input.installation_scope
                || managed.try_get::<String, _>("distribution_adapter").map_err(map_sqlx)? != input.distribution_adapter
            { return Err(integrity_error()); }
            let version = sqlx::query("SELECT manifest_digest,artifact_digest,materialized_tree_digest FROM managed_versions WHERE managed_mcp_id = ? AND version = ?")
                .bind(input.managed_mcp_id).bind(input.version).fetch_one(&mut **tx).await.map_err(map_sqlx)?;
            if version.try_get::<String, _>("manifest_digest").map_err(map_sqlx)? != input.manifest_digest
                || version.try_get::<Option<String>, _>("artifact_digest").map_err(map_sqlx)?.as_deref() != Some(&input.verification_evidence.artifact_digest)
                || version.try_get::<Option<String>, _>("materialized_tree_digest").map_err(map_sqlx)?.as_deref() != Some(input.materialized_tree_digest)
            { return Err(integrity_error()); }
            let created_managed_mcp = previous_version.is_none() && managed.try_get::<Option<String>, _>("owner_task_id").map_err(map_sqlx)?.as_deref() == Some(input.task_id);
            let created_version = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM installation_ownership WHERE managed_mcp_id = ? AND version = ? AND owner_task_id = ?)")
                .bind(input.managed_mcp_id).bind(input.version).bind(input.task_id).fetch_one(&mut **tx).await.map_err(map_sqlx)?;
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(StageManagedInstallationOutcome { record: self.get_managed_inventory(input.managed_mcp_id).await?, created_managed_mcp, created_version, previous_version });
        }
        let existing = sqlx::query("SELECT managed_mcp_id,state_json,active_version,distribution_adapter FROM managed_mcps WHERE mcp_id = ? AND installation_scope = ?")
            .bind(input.mcp_id).bind(input.installation_scope).fetch_optional(&mut **tx).await.map_err(map_sqlx)?;
        let created_managed_mcp = existing.is_none();
        let (previous_version, mut state) = if let Some(row) = existing {
            if row
                .try_get::<String, _>("managed_mcp_id")
                .map_err(map_sqlx)?
                != input.managed_mcp_id
                || row
                    .try_get::<String, _>("distribution_adapter")
                    .map_err(map_sqlx)?
                    != input.distribution_adapter
            {
                return Err(integrity_error());
            }
            (
                row.try_get::<Option<String>, _>("active_version")
                    .map_err(map_sqlx)?,
                decode::<ManagedMcpState>(
                    &row.try_get::<String, _>("state_json").map_err(map_sqlx)?,
                )?,
            )
        } else {
            let state = ManagedMcpState {
                registration: RegistrationState::Registered,
                ..ManagedMcpState::default()
            };
            sqlx::query(r#"INSERT INTO managed_mcps (managed_mcp_id,mcp_id,installation_scope,state_json,revision,created_at_ms,updated_at_ms,distribution_adapter,active_manifest_digest,active_version,owner_task_id) VALUES (?,?,?,?,0,?,?,?,NULL,NULL,?)"#)
                .bind(input.managed_mcp_id).bind(input.mcp_id).bind(input.installation_scope).bind(encode(&state)?).bind(input.now_ms).bind(input.now_ms).bind(input.distribution_adapter).bind(input.task_id)
                .execute(&mut **tx).await.map_err(map_sqlx)?;
            (None, state)
        };
        let version_exists = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM managed_versions WHERE managed_mcp_id = ? AND version = ?)")
            .bind(input.managed_mcp_id).bind(input.version).fetch_one(&mut **tx).await.map_err(map_sqlx)?;
        if !version_exists {
            sqlx::query(r#"INSERT INTO managed_versions (managed_mcp_id,version,manifest_digest,installation_root,verified,active,adapter_evidence_json,created_at_ms,artifact_digest,verification_evidence_json,materialized_tree_digest,activation_state,supply_chain_evidence_json) VALUES (?,?,?,?,1,0,?,?,?,?,?, 'staged',?)"#)
                .bind(input.managed_mcp_id).bind(input.version).bind(input.manifest_digest).bind(input.installation_root).bind(encode(input.adapter_evidence)?).bind(input.now_ms).bind(&input.verification_evidence.artifact_digest).bind(encode(input.verification_evidence)?).bind(input.materialized_tree_digest).bind(input.supply_chain_evidence.map(encode).transpose()?)
                .execute(&mut **tx).await.map_err(map_sqlx)?;
            for path in input.owned_relative_paths {
                let expected_digest = (path == ".").then_some(input.materialized_tree_digest);
                sqlx::query(r#"INSERT INTO installation_ownership (managed_mcp_id,version,relative_path,path_kind,expected_digest,owner_task_id,remove_on_uninstall) VALUES (?,?,?,'directory',?,?,1)"#)
                    .bind(input.managed_mcp_id).bind(input.version).bind(path).bind(expected_digest).bind(input.task_id).execute(&mut **tx).await.map_err(map_sqlx)?;
            }
            sqlx::query(r#"INSERT INTO artifact_cache (artifact_digest,source_origin,size_bytes,adapter_id,adapter_version,platform_selector,verification_evidence_json,reference_count,verified_at_ms) VALUES (?,?,?,?,?,?,?,1,?) ON CONFLICT(artifact_digest) DO UPDATE SET reference_count = reference_count + 1, verification_evidence_json = excluded.verification_evidence_json, verified_at_ms = excluded.verified_at_ms"#)
                .bind(&input.verification_evidence.artifact_digest).bind(&input.verification_evidence.source_origin).bind(i64::try_from(input.verification_evidence.size_bytes).map_err(|_| integrity_error())?).bind(&input.verification_evidence.adapter_id).bind(&input.verification_evidence.adapter_version).bind(&input.verification_evidence.platform_selector).bind(encode(input.verification_evidence)?).bind(input.now_ms)
                .execute(&mut **tx).await.map_err(map_sqlx)?;
        } else if previous_version.as_deref() == Some(input.version) {
            let existing_digest = sqlx::query_scalar::<_, Option<String>>("SELECT artifact_digest FROM managed_versions WHERE managed_mcp_id = ? AND version = ? AND active = 1")
                .bind(input.managed_mcp_id).bind(input.version).fetch_one(&mut **tx).await.map_err(map_sqlx)?;
            if existing_digest.as_deref() != Some(&input.verification_evidence.artifact_digest) {
                return Err(integrity_error());
            }
            let persisted_evidence = sqlx::query_scalar::<_, Option<String>>("SELECT supply_chain_evidence_json FROM managed_versions WHERE managed_mcp_id = ? AND version = ? AND active = 1")
                .bind(input.managed_mcp_id).bind(input.version).fetch_one(&mut **tx).await.map_err(map_sqlx)?;
            if !same_supply_chain_authority(
                persisted_evidence.as_deref(),
                input.supply_chain_evidence,
            )? {
                return Err(integrity_error());
            }
            sqlx::query("UPDATE managed_versions SET installation_root = ?,verified = 1,adapter_evidence_json = ?,verification_evidence_json = ?,materialized_tree_digest = ?,activation_state = 'staged' WHERE managed_mcp_id = ? AND version = ? AND active = 1")
                .bind(input.installation_root).bind(encode(input.adapter_evidence)?).bind(encode(input.verification_evidence)?).bind(input.materialized_tree_digest).bind(input.managed_mcp_id).bind(input.version).execute(&mut **tx).await.map_err(map_sqlx)?;
            sqlx::query("UPDATE installation_ownership SET expected_digest = ? WHERE managed_mcp_id = ? AND version = ? AND relative_path = '.'")
                .bind(input.materialized_tree_digest).bind(input.managed_mcp_id).bind(input.version).execute(&mut **tx).await.map_err(map_sqlx)?;
        } else {
            let existing = sqlx::query("SELECT manifest_digest,artifact_digest,materialized_tree_digest,supply_chain_evidence_json,verified FROM managed_versions WHERE managed_mcp_id = ? AND version = ?")
                .bind(input.managed_mcp_id).bind(input.version).fetch_one(&mut **tx).await.map_err(map_sqlx)?;
            if existing
                .try_get::<String, _>("manifest_digest")
                .map_err(map_sqlx)?
                != input.manifest_digest
                || existing
                    .try_get::<Option<String>, _>("artifact_digest")
                    .map_err(map_sqlx)?
                    .as_deref()
                    != Some(&input.verification_evidence.artifact_digest)
                || !existing.try_get::<bool, _>("verified").map_err(map_sqlx)?
                || existing
                    .try_get::<Option<String>, _>("materialized_tree_digest")
                    .map_err(map_sqlx)?
                    .as_deref()
                    != Some(input.materialized_tree_digest)
                || !same_supply_chain_authority(
                    existing
                        .try_get::<Option<String>, _>("supply_chain_evidence_json")
                        .map_err(map_sqlx)?
                        .as_deref(),
                    input.supply_chain_evidence,
                )?
            {
                return Err(integrity_error());
            }
            sqlx::query("UPDATE managed_versions SET activation_state = 'staged' WHERE managed_mcp_id = ? AND version = ?")
                .bind(input.managed_mcp_id).bind(input.version).execute(&mut **tx).await.map_err(map_sqlx)?;
        }
        let previous_state_json = encode(&state)?;
        state.installation = InstallationState::Staged;
        sqlx::query("UPDATE managed_mcps SET state_json = ?, updated_at_ms = ?, revision = revision + 1 WHERE managed_mcp_id = ?")
            .bind(encode(&state)?).bind(input.now_ms).bind(input.managed_mcp_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        let existing_activation = sqlx::query("SELECT previous_version,target_version,previous_state_json FROM activation_journal WHERE managed_mcp_id = ? AND task_id = ?")
            .bind(input.managed_mcp_id).bind(input.task_id).fetch_optional(&mut **tx).await.map_err(map_sqlx)?;
        if let Some(row) = existing_activation {
            if row
                .try_get::<Option<String>, _>("previous_version")
                .map_err(map_sqlx)?
                != previous_version
                || row
                    .try_get::<String, _>("target_version")
                    .map_err(map_sqlx)?
                    != input.version
                || row
                    .try_get::<String, _>("previous_state_json")
                    .map_err(map_sqlx)?
                    != previous_state_json
            {
                return Err(integrity_error());
            }
        } else {
            sqlx::query("INSERT INTO activation_journal(managed_mcp_id,task_id,previous_version,target_version,previous_state_json,status,created_at_ms,updated_at_ms) VALUES (?,?,?,?,?,'started',?,?)")
                .bind(input.managed_mcp_id).bind(input.task_id).bind(&previous_version).bind(input.version).bind(previous_state_json).bind(input.now_ms).bind(input.now_ms).execute(&mut **tx).await.map_err(map_sqlx)?;
        }
        tx.commit().await.map_err(map_sqlx)?;
        Ok(StageManagedInstallationOutcome {
            record: self.get_managed_inventory(input.managed_mcp_id).await?,
            created_managed_mcp,
            created_version: !version_exists,
            previous_version,
        })
    }

    pub(crate) async fn activate_managed_installation(
        &self,
        input: ActivateManagedInstallation<'_>,
    ) -> McpPlatformResult<ManagedMcpInventoryRecord> {
        let mut tx = self.begin_immediate().await?;
        let journal = sqlx::query("SELECT previous_version,target_version,status FROM activation_journal WHERE managed_mcp_id = ? AND task_id = ?")
            .bind(input.managed_mcp_id).bind(input.task_id).fetch_optional(&mut **tx).await.map_err(map_sqlx)?.ok_or_else(not_found)?;
        if journal
            .try_get::<String, _>("target_version")
            .map_err(map_sqlx)?
            != input.target_version
        {
            return Err(integrity_error());
        }
        let status: String = journal.try_get("status").map_err(map_sqlx)?;
        if status == "health_committed" {
            tx.commit().await.map_err(map_sqlx)?;
            return self.get_managed_inventory(input.managed_mcp_id).await;
        }
        if status != "pointer_committed" {
            return Err(integrity_error());
        }
        let target = sqlx::query("SELECT manifest_digest,activation_state FROM managed_versions WHERE managed_mcp_id = ? AND version = ?")
            .bind(input.managed_mcp_id).bind(input.target_version).fetch_optional(&mut **tx).await.map_err(map_sqlx)?.ok_or_else(not_found)?;
        if target
            .try_get::<String, _>("activation_state")
            .map_err(map_sqlx)?
            != "staged"
        {
            return Err(integrity_error());
        }
        let previous_version = journal
            .try_get::<Option<String>, _>("previous_version")
            .map_err(map_sqlx)?;
        if let Some(previous) = previous_version
            .as_deref()
            .filter(|previous| *previous != input.target_version)
        {
            sqlx::query("UPDATE managed_versions SET active = 0, activation_state = 'retained' WHERE managed_mcp_id = ? AND version = ? AND active = 1")
                .bind(input.managed_mcp_id).bind(previous).execute(&mut **tx).await.map_err(map_sqlx)?;
        }
        sqlx::query("UPDATE managed_versions SET active = 1, activation_state = 'active' WHERE managed_mcp_id = ? AND version = ?")
            .bind(input.managed_mcp_id).bind(input.target_version).execute(&mut **tx).await.map_err(map_sqlx)?;
        let state_json = sqlx::query_scalar::<_, String>(
            "SELECT state_json FROM managed_mcps WHERE managed_mcp_id = ?",
        )
        .bind(input.managed_mcp_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let mut state = decode::<ManagedMcpState>(&state_json)?;
        state.installation = InstallationState::Installed;
        let manifest_digest: String = target.try_get("manifest_digest").map_err(map_sqlx)?;
        sqlx::query("UPDATE managed_mcps SET state_json = ?, active_manifest_digest = ?, active_version = ?, updated_at_ms = ?, revision = revision + 1 WHERE managed_mcp_id = ?")
            .bind(encode(&state)?).bind(manifest_digest).bind(input.target_version).bind(input.now_ms).bind(input.managed_mcp_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        sqlx::query("UPDATE activation_journal SET status = 'health_committed', updated_at_ms = ? WHERE managed_mcp_id = ? AND task_id = ? AND status = 'pointer_committed'")
            .bind(input.now_ms).bind(input.managed_mcp_id).bind(input.task_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_managed_inventory(input.managed_mcp_id).await
    }

    pub(crate) async fn rollback_managed_installation(
        &self,
        managed_mcp_id: &str,
        previous_version: Option<&str>,
        target_version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let journal = sqlx::query("SELECT previous_version,target_version,previous_state_json,status FROM activation_journal WHERE managed_mcp_id = ? AND task_id = ?")
            .bind(managed_mcp_id).bind(task_id).fetch_optional(&mut **tx).await.map_err(map_sqlx)?.ok_or_else(not_found)?;
        if journal
            .try_get::<Option<String>, _>("previous_version")
            .map_err(map_sqlx)?
            .as_deref()
            != previous_version
            || journal
                .try_get::<String, _>("target_version")
                .map_err(map_sqlx)?
                != target_version
        {
            return Err(integrity_error());
        }
        if journal.try_get::<String, _>("status").map_err(map_sqlx)? == "rolled_back" {
            if previous_version.is_none() {
                sqlx::query("DELETE FROM managed_mcps WHERE managed_mcp_id = ? AND owner_task_id = ? AND NOT EXISTS(SELECT 1 FROM managed_versions WHERE managed_mcp_id = ?) AND NOT EXISTS(SELECT 1 FROM connection_projections WHERE managed_mcp_id = ?)")
                    .bind(managed_mcp_id).bind(task_id).bind(managed_mcp_id).bind(managed_mcp_id).execute(&mut **tx).await.map_err(map_sqlx)?;
            }
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(());
        }
        if previous_version == Some(target_version) {
            let state_json: String = journal.try_get("previous_state_json").map_err(map_sqlx)?;
            let state = decode::<ManagedMcpState>(&state_json)?;
            sqlx::query("UPDATE managed_versions SET active = 1,activation_state = 'active' WHERE managed_mcp_id = ? AND version = ?").bind(managed_mcp_id).bind(target_version).execute(&mut **tx).await.map_err(map_sqlx)?;
            sqlx::query("UPDATE managed_mcps SET state_json = ?,updated_at_ms = ?,revision = revision + 1 WHERE managed_mcp_id = ?").bind(encode(&state)?).bind(now_ms).bind(managed_mcp_id).execute(&mut **tx).await.map_err(map_sqlx)?;
            sqlx::query("UPDATE activation_journal SET status = 'rolled_back',updated_at_ms = ? WHERE managed_mcp_id = ? AND task_id = ?").bind(now_ms).bind(managed_mcp_id).bind(task_id).execute(&mut **tx).await.map_err(map_sqlx)?;
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(());
        }
        let owned = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM installation_ownership WHERE managed_mcp_id = ? AND version = ? AND owner_task_id = ?)")
            .bind(managed_mcp_id).bind(target_version).bind(task_id).fetch_one(&mut **tx).await.map_err(map_sqlx)?;
        if !owned {
            return Err(integrity_error());
        }
        let artifact_digest = sqlx::query_scalar::<_, Option<String>>(
            "SELECT artifact_digest FROM managed_versions WHERE managed_mcp_id = ? AND version = ?",
        )
        .bind(managed_mcp_id)
        .bind(target_version)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .flatten();
        sqlx::query("UPDATE managed_versions SET active = 0, activation_state = 'removal_pending' WHERE managed_mcp_id = ? AND version = ?")
            .bind(managed_mcp_id).bind(target_version).execute(&mut **tx).await.map_err(map_sqlx)?;
        if let Some(previous) = previous_version {
            sqlx::query("UPDATE managed_versions SET active = 1, activation_state = 'active' WHERE managed_mcp_id = ? AND version = ?")
                .bind(managed_mcp_id).bind(previous).execute(&mut **tx).await.map_err(map_sqlx)?;
            let digest = sqlx::query_scalar::<_, String>("SELECT manifest_digest FROM managed_versions WHERE managed_mcp_id = ? AND version = ?").bind(managed_mcp_id).bind(previous).fetch_one(&mut **tx).await.map_err(map_sqlx)?;
            let state_json: String = journal.try_get("previous_state_json").map_err(map_sqlx)?;
            let state = decode::<ManagedMcpState>(&state_json)?;
            sqlx::query("UPDATE managed_mcps SET state_json = ?,active_manifest_digest = ?,active_version = ?,updated_at_ms = ?,revision = revision + 1 WHERE managed_mcp_id = ?")
                .bind(encode(&state)?).bind(digest).bind(previous).bind(now_ms).bind(managed_mcp_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        }
        sqlx::query("DELETE FROM installation_ownership WHERE managed_mcp_id = ? AND version = ? AND owner_task_id = ?").bind(managed_mcp_id).bind(target_version).bind(task_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        sqlx::query(
            "DELETE FROM managed_versions WHERE managed_mcp_id = ? AND version = ? AND active = 0",
        )
        .bind(managed_mcp_id)
        .bind(target_version)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if let Some(digest) = artifact_digest {
            sqlx::query("UPDATE artifact_cache SET reference_count = reference_count - 1 WHERE artifact_digest = ? AND reference_count > 0").bind(digest).execute(&mut **tx).await.map_err(map_sqlx)?;
        }
        sqlx::query("UPDATE activation_journal SET status = 'rolled_back',updated_at_ms = ? WHERE managed_mcp_id = ? AND task_id = ?").bind(now_ms).bind(managed_mcp_id).bind(task_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        if previous_version.is_none() {
            sqlx::query("DELETE FROM managed_mcps WHERE managed_mcp_id = ? AND owner_task_id = ? AND NOT EXISTS(SELECT 1 FROM managed_versions WHERE managed_mcp_id = ?)").bind(managed_mcp_id).bind(task_id).bind(managed_mcp_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        }
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn begin_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<ManagedUninstallSnapshot> {
        let mut tx = self.begin_immediate().await?;
        if let Some(row) = sqlx::query("SELECT managed_mcp_id,version,artifact_digest FROM uninstall_journal WHERE task_id = ?").bind(task_id).fetch_optional(&mut **tx).await.map_err(map_sqlx)? {
            let snapshot = ManagedUninstallSnapshot { managed_mcp_id: row.try_get("managed_mcp_id").map_err(map_sqlx)?, version: row.try_get("version").map_err(map_sqlx)?, artifact_digest: row.try_get("artifact_digest").map_err(map_sqlx)? };
            if snapshot.managed_mcp_id != managed_mcp_id || snapshot.version != version { return Err(integrity_error()); }
            tx.commit().await.map_err(map_sqlx)?; return Ok(snapshot);
        }
        let lease_owner = sqlx::query_scalar::<_, String>(
            "SELECT task_id FROM managed_lifecycle_leases WHERE managed_mcp_id = ? AND expires_at_ms > ?",
        )
        .bind(managed_mcp_id)
        .bind(now_ms)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if lease_owner.as_deref() != Some(task_id) {
            return Err(crate::mcp_platform::error::McpPlatformError::new(
                McpPlatformErrorCode::PlanConflict,
                "managed MCP lifecycle lease is not owned by this task",
            ));
        }
        let uninstall_pending = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM uninstall_journal WHERE managed_mcp_id = ? AND status IN ('started','quarantined'))")
            .bind(managed_mcp_id).fetch_one(&mut **tx).await.map_err(map_sqlx)?;
        if uninstall_pending {
            return Err(crate::mcp_platform::error::McpPlatformError::new(
                McpPlatformErrorCode::PlanConflict,
                "managed MCP already has an active lifecycle mutation",
            ));
        }
        let row = sqlx::query(
            "SELECT state_json,active_version FROM managed_mcps WHERE managed_mcp_id = ?",
        )
        .bind(managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        if row
            .try_get::<Option<String>, _>("active_version")
            .map_err(map_sqlx)?
            .as_deref()
            != Some(version)
        {
            return Err(integrity_error());
        }
        let previous_state_json: String = row.try_get("state_json").map_err(map_sqlx)?;
        let mut state = decode::<ManagedMcpState>(&previous_state_json)?;
        state.installation = InstallationState::UninstallPending;
        let artifact_digest = sqlx::query_scalar::<_, Option<String>>("SELECT artifact_digest FROM managed_versions WHERE managed_mcp_id = ? AND version = ? AND active = 1").bind(managed_mcp_id).bind(version).fetch_one(&mut **tx).await.map_err(map_sqlx)?;
        sqlx::query("INSERT INTO uninstall_journal(task_id,managed_mcp_id,version,previous_state_json,artifact_digest,status,created_at_ms,updated_at_ms) VALUES (?,?,?,?,?,'started',?,?)")
            .bind(task_id).bind(managed_mcp_id).bind(version).bind(previous_state_json).bind(&artifact_digest).bind(now_ms).bind(now_ms).execute(&mut **tx).await.map_err(map_sqlx)?;
        sqlx::query("UPDATE managed_mcps SET state_json = ?,updated_at_ms = ?,revision = revision + 1 WHERE managed_mcp_id = ?").bind(encode(&state)?).bind(now_ms).bind(managed_mcp_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(ManagedUninstallSnapshot {
            managed_mcp_id: managed_mcp_id.to_string(),
            version: version.to_string(),
            artifact_digest,
        })
    }

    pub(crate) async fn mark_managed_uninstall_quarantined(
        &self,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        if sqlx::query_scalar::<_, String>("SELECT status FROM uninstall_journal WHERE task_id = ?")
            .bind(task_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_sqlx)?
            .as_deref()
            == Some("committed")
        {
            tx.rollback().await?;
            return Ok(());
        }
        let result = sqlx::query("UPDATE uninstall_journal SET status = 'quarantined',updated_at_ms = ? WHERE task_id = ? AND status IN ('started','quarantined')").bind(now_ms).bind(task_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            return Err(integrity_error());
        }
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn cancel_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let row = sqlx::query("SELECT previous_state_json,status FROM uninstall_journal WHERE task_id = ? AND managed_mcp_id = ?").bind(task_id).bind(managed_mcp_id).fetch_optional(&mut **tx).await.map_err(map_sqlx)?.ok_or_else(not_found)?;
        let status: String = row.try_get("status").map_err(map_sqlx)?;
        if status == "cancelled" {
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(());
        }
        if !matches!(status.as_str(), "started" | "quarantined") {
            return Err(integrity_error());
        }
        let previous: String = row.try_get("previous_state_json").map_err(map_sqlx)?;
        sqlx::query("UPDATE managed_mcps SET state_json = ?,updated_at_ms = ?,revision = revision + 1 WHERE managed_mcp_id = ?").bind(previous).bind(now_ms).bind(managed_mcp_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE uninstall_journal SET status = 'cancelled',updated_at_ms = ? WHERE task_id = ?",
        )
        .bind(now_ms)
        .bind(task_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn finalize_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let row = sqlx::query("SELECT status,artifact_digest FROM uninstall_journal WHERE task_id = ? AND managed_mcp_id = ? AND version = ?").bind(task_id).bind(managed_mcp_id).bind(version).fetch_optional(&mut **tx).await.map_err(map_sqlx)?.ok_or_else(not_found)?;
        let status: String = row.try_get("status").map_err(map_sqlx)?;
        if status == "committed" {
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(());
        }
        if status != "quarantined" {
            return Err(integrity_error());
        }
        let version_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM managed_versions WHERE managed_mcp_id = ?",
        )
        .bind(managed_mcp_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let owned_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(DISTINCT version) FROM installation_ownership WHERE managed_mcp_id = ? AND remove_on_uninstall = 1").bind(managed_mcp_id).fetch_one(&mut **tx).await.map_err(map_sqlx)?;
        if version_count == 0 || owned_count != version_count {
            return Err(integrity_error());
        }
        let digests = sqlx::query_scalar::<_, Option<String>>(
            "SELECT artifact_digest FROM managed_versions WHERE managed_mcp_id = ?",
        )
        .bind(managed_mcp_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let projection = sqlx::query(
            "SELECT link_key,projection_digest,revision,plan_id,manifest_digest,owner_task_id FROM connection_projections WHERE managed_mcp_id=?",
        )
        .bind(managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if let Some(projection) = projection {
            let mac = sqlx::query_scalar::<_, String>(
                "SELECT mac FROM managed_projection_integrity WHERE managed_mcp_id=?",
            )
            .bind(managed_mcp_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(integrity_error)?;
            let plan_id: Option<String> = projection.try_get("plan_id").map_err(map_sqlx)?;
            let manifest_digest: Option<String> =
                projection.try_get("manifest_digest").map_err(map_sqlx)?;
            let owner_task_id: Option<String> =
                projection.try_get("owner_task_id").map_err(map_sqlx)?;
            self.integrity_signer.verify(
                "managed-projection",
                &managed_projection_integrity_payload(
                    managed_mcp_id,
                    &projection
                        .try_get::<String, _>("link_key")
                        .map_err(map_sqlx)?,
                    &projection
                        .try_get::<String, _>("projection_digest")
                        .map_err(map_sqlx)?,
                    projection.try_get("revision").map_err(map_sqlx)?,
                    plan_id.as_deref(),
                    manifest_digest.as_deref(),
                    owner_task_id.as_deref(),
                ),
                &mac,
            )?;
            sqlx::query("DELETE FROM managed_projection_integrity WHERE managed_mcp_id=?")
                .bind(managed_mcp_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
            sqlx::query("DELETE FROM projection_writer_bindings WHERE managed_mcp_id=?")
                .bind(managed_mcp_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
        }
        sqlx::query("DELETE FROM connection_projections WHERE managed_mcp_id = ?")
            .bind(managed_mcp_id)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        sqlx::query("DELETE FROM installation_ownership WHERE managed_mcp_id = ?")
            .bind(managed_mcp_id)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        sqlx::query("DELETE FROM managed_versions WHERE managed_mcp_id = ?")
            .bind(managed_mcp_id)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        sqlx::query("DELETE FROM managed_mcps WHERE managed_mcp_id = ? AND NOT EXISTS(SELECT 1 FROM managed_versions WHERE managed_mcp_id = ?)").bind(managed_mcp_id).bind(managed_mcp_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        for digest in digests.into_iter().flatten() {
            sqlx::query("UPDATE artifact_cache SET reference_count = reference_count - 1 WHERE artifact_digest = ? AND reference_count > 0").bind(digest).execute(&mut **tx).await.map_err(map_sqlx)?;
        }
        sqlx::query(
            "UPDATE uninstall_journal SET status = 'committed',updated_at_ms = ? WHERE task_id = ?",
        )
        .bind(now_ms)
        .bind(task_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn register_managed_mcp(
        &self,
        input: RegisterManagedMcp<'_>,
    ) -> McpPlatformResult<RegisterManagedMcpOutcome> {
        let mut tx = self.begin_immediate().await?;
        let manifest =
            sqlx::query("SELECT mcp_id, version FROM manifest_blobs WHERE manifest_digest = ?")
                .bind(input.manifest_digest)
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx)?
                .ok_or_else(not_found)?;
        if manifest.try_get::<String, _>("mcp_id").map_err(map_sqlx)? != input.mcp_id
            || manifest.try_get::<String, _>("version").map_err(map_sqlx)? != input.version
        {
            return Err(integrity_error());
        }

        if let Some(row) = sqlx::query(
            r#"SELECT managed_mcp_id, distribution_adapter, active_manifest_digest,
                active_version FROM managed_mcps WHERE mcp_id = ? AND installation_scope = ?"#,
        )
        .bind(input.mcp_id)
        .bind(input.installation_scope)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        {
            let managed_mcp_id: String = row.try_get("managed_mcp_id").map_err(map_sqlx)?;
            let matches = managed_mcp_id == input.managed_mcp_id
                && row
                    .try_get::<String, _>("distribution_adapter")
                    .map_err(map_sqlx)?
                    == input.distribution_adapter
                && row
                    .try_get::<Option<String>, _>("active_manifest_digest")
                    .map_err(map_sqlx)?
                    .as_deref()
                    == Some(input.manifest_digest)
                && row
                    .try_get::<Option<String>, _>("active_version")
                    .map_err(map_sqlx)?
                    .as_deref()
                    == Some(input.version);
            tx.commit().await.map_err(map_sqlx)?;
            if !matches {
                return Err(error(
                    McpPlatformErrorCode::IdempotencyConflict,
                    "managed MCP identity already refers to different immutable content",
                ));
            }
            return Ok(RegisterManagedMcpOutcome {
                record: self.get_managed_inventory(input.managed_mcp_id).await?,
                created: false,
            });
        }

        let state = ManagedMcpState {
            registration: RegistrationState::Registered,
            installation: InstallationState::NotApplicable,
            ..ManagedMcpState::default()
        };
        sqlx::query(
            r#"INSERT INTO managed_mcps (
                managed_mcp_id, mcp_id, installation_scope, state_json, revision,
                created_at_ms, updated_at_ms, distribution_adapter, active_manifest_digest,
                active_version, owner_task_id
            ) VALUES (?, ?, ?, ?, 0, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(input.managed_mcp_id)
        .bind(input.mcp_id)
        .bind(input.installation_scope)
        .bind(encode(&state)?)
        .bind(input.now_ms)
        .bind(input.now_ms)
        .bind(input.distribution_adapter)
        .bind(input.manifest_digest)
        .bind(input.version)
        .bind(input.task_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            r#"INSERT INTO managed_versions (
                managed_mcp_id, version, manifest_digest, installation_root, verified, active,
                adapter_evidence_json, created_at_ms
            ) VALUES (?, ?, ?, NULL, 1, 1, ?, ?)"#,
        )
        .bind(input.managed_mcp_id)
        .bind(input.version)
        .bind(input.manifest_digest)
        .bind(encode(input.adapter_evidence)?)
        .bind(input.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(RegisterManagedMcpOutcome {
            record: self.get_managed_inventory(input.managed_mcp_id).await?,
            created: true,
        })
    }

    pub async fn get_managed_inventory(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ManagedMcpInventoryRecord> {
        let mut tx = self.begin_verified_read_snapshot().await?;
        let inventory = Self::fetch_managed_inventory_snapshot(&mut tx, managed_mcp_id).await?;
        tx.rollback().await.map_err(map_sqlx)?;
        Ok(inventory)
    }

    pub(super) async fn fetch_managed_inventory_snapshot(
        tx: &mut Transaction<'_, Sqlite>,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ManagedMcpInventoryRecord> {
        let row = sqlx::query(
            r#"SELECT managed_mcp_id,mcp_id,installation_scope,state_json,revision,
                       created_at_ms,updated_at_ms,distribution_adapter,
                       active_manifest_digest,active_version,owner_task_id
                FROM managed_mcps WHERE managed_mcp_id = ?"#,
        )
        .bind(managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let version_rows = sqlx::query(
            r#"SELECT version,manifest_digest,installation_root,verified,active,
                      adapter_evidence_json,materialized_tree_digest,supply_chain_evidence_json,
                      created_at_ms FROM managed_versions
               WHERE managed_mcp_id=? ORDER BY version"#,
        )
        .bind(managed_mcp_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let versions = version_rows
            .iter()
            .map(decode_managed_version_row)
            .collect::<McpPlatformResult<Vec<_>>>()?;
        let managed = decode_managed_row(&row, versions)?;
        let lifecycle = ManagedMcpLifecycleMetadata {
            distribution_adapter: row.try_get("distribution_adapter").map_err(map_sqlx)?,
            active_manifest_digest: row.try_get("active_manifest_digest").map_err(map_sqlx)?,
            active_version: row.try_get("active_version").map_err(map_sqlx)?,
            owner_task_id: row.try_get("owner_task_id").map_err(map_sqlx)?,
        };
        match (
            lifecycle.active_manifest_digest.as_deref(),
            lifecycle.active_version.as_deref(),
        ) {
            (Some(digest), Some(version)) => {
                let active = managed
                    .versions
                    .iter()
                    .find(|candidate| candidate.active)
                    .ok_or_else(integrity_error)?;
                if active.manifest_digest != digest || active.version != version {
                    return Err(integrity_error());
                }
                let manifest_row = sqlx::query(
                    r#"SELECT manifest_digest,mcp_id,version,canonical_bytes,proof_json,
                              trust_tier_json,source_ref_json,import_kind,release_id,
                              origin_provenance_json,update_channel_json,created_at_ms
                       FROM manifest_blobs WHERE manifest_digest=?"#,
                )
                .bind(digest)
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx)?
                .ok_or_else(not_found)?;
                let manifest = decode_manifest_row(&manifest_row)?;
                if manifest.verified.manifest().id != managed.mcp_id
                    || manifest.verified.manifest().version.as_str() != version
                {
                    return Err(integrity_error());
                }
            }
            (None, None) => {}
            _ => return Err(integrity_error()),
        }
        Ok(ManagedMcpInventoryRecord { managed, lifecycle })
    }

    pub async fn list_managed_inventory(
        &self,
        after_managed_mcp_id: Option<&str>,
        limit: usize,
        filter: &super::super::ManagedInventoryFilter,
    ) -> McpPlatformResult<Vec<ManagedMcpInventoryRecord>> {
        let ids = sqlx::query_scalar::<_, String>(
            r#"SELECT managed_mcp_id FROM managed_mcps
                WHERE (? IS NULL OR managed_mcp_id > ?)
                  AND (? IS NULL OR json_extract(state_json, '$.registration') = ?)
                  AND (? IS NULL OR json_extract(state_json, '$.installation') = ?)
                  AND (? IS NULL OR json_extract(state_json, '$.runtime') = ?)
                  AND (? IS NULL OR json_extract(state_json, '$.health') = ?)
                  AND (? IS NULL OR json_extract(state_json, '$.default_enabled') = ?)
                ORDER BY managed_mcp_id LIMIT ?"#,
        )
        .bind(after_managed_mcp_id)
        .bind(after_managed_mcp_id)
        .bind(filter.registration.map(enum_json).transpose()?)
        .bind(filter.registration.map(enum_json).transpose()?)
        .bind(filter.installation.map(enum_json).transpose()?)
        .bind(filter.installation.map(enum_json).transpose()?)
        .bind(filter.runtime.map(enum_json).transpose()?)
        .bind(filter.runtime.map(enum_json).transpose()?)
        .bind(filter.health.map(enum_json).transpose()?)
        .bind(filter.health.map(enum_json).transpose()?)
        .bind(filter.default_enabled.map(i64::from))
        .bind(filter.default_enabled.map(i64::from))
        .bind(i64::try_from(limit).map_err(|_| integrity_error())?)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        let mut records = Vec::with_capacity(ids.len());
        for id in ids {
            records.push(self.get_managed_inventory(&id).await?);
        }
        Ok(records)
    }

    pub(crate) async fn put_owned_connection_projection(
        &self,
        input: PutOwnedProjection<'_>,
    ) -> McpPlatformResult<ConnectionProjectionRecord> {
        let mut tx = self.begin_immediate().await?;
        let plan = fetch_valid_plan(&mut tx, input.plan_id).await?;
        let scope = plan.target.installation_scope.as_deref().unwrap_or("user");
        let managed_mcp_id =
            crate::mcp_platform::task_runner::stable_managed_mcp_id(plan.plan.manifest_id(), scope);
        let link_key = format!(
            "managed_mcp_{}",
            managed_mcp_id
                .strip_prefix("managed_")
                .ok_or_else(integrity_error)?
        );
        let projection = plan.plan.connection_projection();
        let projection_digest = crate::mcp_platform::projection_config_digest(projection)?;
        let projection_json = crate::mcp_platform::encode_projection_config(projection)?;
        let owner_task = fetch_task(&mut tx, input.owner_task_id).await?;
        if plan.target.mcp_id != plan.plan.manifest_id()
            || plan
                .target
                .managed_mcp_id
                .as_deref()
                .is_some_and(|id| id != managed_mcp_id)
            || owner_task.plan_id != input.plan_id
            || owner_task.plan_digest != plan.plan.plan_digest()
            || owner_task.operation != plan.plan.operation().into()
        {
            return Err(integrity_error());
        }
        let worker_lease_expires_at_ms = owner_task
            .lease_expires_at_ms
            .filter(|expires_at_ms| *expires_at_ms > input.now_ms)
            .ok_or_else(integrity_error)?;
        if owner_task.owner_id.as_deref() != Some(input.worker_owner_id)
            || !matches!(
                owner_task.status,
                TaskStatus::Running | TaskStatus::Verifying | TaskStatus::Activating
            )
        {
            return Err(integrity_error());
        }
        let lifecycle_acquired_at_ms = sqlx::query_scalar::<_, i64>(
            "SELECT acquired_at_ms FROM managed_lifecycle_leases WHERE managed_mcp_id=? AND task_id=? AND expires_at_ms>?",
        )
        .bind(&managed_mcp_id)
        .bind(input.owner_task_id)
        .bind(input.now_ms)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(integrity_error)?;
        let writer_step = fetch_task_step(
            &mut tx,
            self.integrity_signer.as_ref(),
            input.owner_task_id,
            input.step_ordinal,
        )
        .await?;
        if writer_step.idempotency_token != input.step_token
            || writer_step.adapter_id != "connection_projection_repository"
            || writer_step.status != crate::mcp_platform::task::TaskStepStatus::Started
        {
            return Err(integrity_error());
        }
        let writer_mac = self.integrity_signer.sign(
            "projection-writer-authorization",
            &projection_writer_payload(
                &managed_mcp_id,
                input.owner_task_id,
                input.plan_id,
                &owner_task.plan_digest,
                &link_key,
                plan.plan.manifest_digest(),
                &projection_digest,
                lifecycle_acquired_at_ms,
                input.worker_owner_id,
                worker_lease_expires_at_ms,
                input.step_ordinal,
                input.step_token,
            ),
        )?;
        let managed_digest = sqlx::query_scalar::<_, Option<String>>(
            "SELECT active_manifest_digest FROM managed_mcps WHERE managed_mcp_id = ?",
        )
        .bind(&managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let digest_allowed = managed_digest.as_deref() == Some(plan.plan.manifest_digest())
            || (plan.target.managed_mcp_id.as_deref() == Some(managed_mcp_id.as_str())
                && matches!(
                    plan.plan.operation(),
                    crate::mcp_platform::policy::PlanOperation::Update
                        | crate::mcp_platform::policy::PlanOperation::Repair
                ));
        if !digest_allowed {
            return Err(integrity_error());
        }
        if let Some(row) = sqlx::query(
            "SELECT * FROM connection_projections WHERE managed_mcp_id = ? OR link_key = ?",
        )
        .bind(&managed_mcp_id)
        .bind(&link_key)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        {
            let current_managed_mcp_id: String = row.try_get("managed_mcp_id").map_err(map_sqlx)?;
            let current_link_key: String = row.try_get("link_key").map_err(map_sqlx)?;
            let current_projection_digest: String =
                row.try_get("projection_digest").map_err(map_sqlx)?;
            let current_revision: i64 = row.try_get("revision").map_err(map_sqlx)?;
            let current_plan_id: Option<String> = row.try_get("plan_id").map_err(map_sqlx)?;
            let current_manifest_digest: Option<String> =
                row.try_get("manifest_digest").map_err(map_sqlx)?;
            let current_owner_task_id: Option<String> =
                row.try_get("owner_task_id").map_err(map_sqlx)?;
            let current_mac = sqlx::query_scalar::<_, String>(
                "SELECT mac FROM managed_projection_integrity WHERE managed_mcp_id=?",
            )
            .bind(&current_managed_mcp_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(integrity_error)?;
            self.integrity_signer.verify(
                "managed-projection",
                &managed_projection_integrity_payload(
                    &current_managed_mcp_id,
                    &current_link_key,
                    &current_projection_digest,
                    current_revision,
                    current_plan_id.as_deref(),
                    current_manifest_digest.as_deref(),
                    current_owner_task_id.as_deref(),
                ),
                &current_mac,
            )?;
            let same = row
                .try_get::<String, _>("managed_mcp_id")
                .map_err(map_sqlx)?
                == managed_mcp_id
                && row.try_get::<String, _>("link_key").map_err(map_sqlx)? == link_key
                && row
                    .try_get::<String, _>("projection_digest")
                    .map_err(map_sqlx)?
                    == projection_digest
                && crate::mcp_platform::decode_verified_projection_config(
                    &row.try_get::<String, _>("projection_json")
                        .map_err(map_sqlx)?,
                    &projection_digest,
                )? == *projection
                && row.try_get::<String, _>("plan_id").map_err(map_sqlx)? == input.plan_id
                && row
                    .try_get::<String, _>("manifest_digest")
                    .map_err(map_sqlx)?
                    == plan.plan.manifest_digest()
                && row
                    .try_get::<Option<String>, _>("owner_task_id")
                    .map_err(map_sqlx)?
                    .as_deref()
                    == Some(input.owner_task_id);
            if same {
                verify_projection_writer_binding(
                    &mut tx,
                    self.integrity_signer.as_ref(),
                    &managed_mcp_id,
                    input.owner_task_id,
                    input.plan_id,
                    &link_key,
                    plan.plan.manifest_digest(),
                    &projection_digest,
                )
                .await?;
                tx.commit().await.map_err(map_sqlx)?;
                return self.get_connection_projection(&managed_mcp_id).await;
            }
            let same_identity = row
                .try_get::<String, _>("managed_mcp_id")
                .map_err(map_sqlx)?
                == managed_mcp_id
                && row.try_get::<String, _>("link_key").map_err(map_sqlx)? == link_key
                && plan.target.managed_mcp_id.as_deref() == Some(managed_mcp_id.as_str())
                && matches!(
                    plan.plan.operation(),
                    crate::mcp_platform::policy::PlanOperation::Update
                        | crate::mcp_platform::policy::PlanOperation::Repair
                );
            if !same_identity {
                return Err(error(
                    McpPlatformErrorCode::ProjectionConflict,
                    "extension projection key is already owned by different content",
                ));
            }
            sqlx::query(r#"INSERT INTO projection_writer_bindings(managed_mcp_id,task_id,plan_id,plan_digest,link_key,manifest_digest,projection_digest,lifecycle_acquired_at_ms,worker_owner_id,worker_lease_expires_at_ms,step_ordinal,step_token,mac) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(managed_mcp_id,task_id) DO UPDATE SET plan_id=excluded.plan_id,plan_digest=excluded.plan_digest,link_key=excluded.link_key,manifest_digest=excluded.manifest_digest,projection_digest=excluded.projection_digest,lifecycle_acquired_at_ms=excluded.lifecycle_acquired_at_ms,worker_owner_id=excluded.worker_owner_id,worker_lease_expires_at_ms=excluded.worker_lease_expires_at_ms,step_ordinal=excluded.step_ordinal,step_token=excluded.step_token,mac=excluded.mac"#)
                .bind(&managed_mcp_id).bind(input.owner_task_id).bind(input.plan_id).bind(&owner_task.plan_digest).bind(&link_key).bind(plan.plan.manifest_digest()).bind(&projection_digest).bind(lifecycle_acquired_at_ms).bind(input.worker_owner_id).bind(worker_lease_expires_at_ms).bind(input.step_ordinal).bind(input.step_token).bind(&writer_mac)
                .execute(&mut **tx).await.map_err(map_sqlx)?;
            let next_revision = current_revision + 1;
            sqlx::query(r#"UPDATE connection_projections SET projection_json = ?, revision = revision + 1, updated_at_ms = ?, plan_id = ?, manifest_digest = ?, owner_task_id = ?, projection_digest = ? WHERE managed_mcp_id = ? AND link_key = ?"#)
                .bind(&projection_json).bind(input.now_ms).bind(input.plan_id).bind(plan.plan.manifest_digest()).bind(input.owner_task_id).bind(&projection_digest).bind(&managed_mcp_id).bind(&link_key)
                .execute(&mut **tx).await.map_err(map_sqlx)?;
            let projection_mac = self.integrity_signer.sign(
                "managed-projection",
                &managed_projection_integrity_payload(
                    &managed_mcp_id,
                    &link_key,
                    &projection_digest,
                    next_revision,
                    Some(input.plan_id),
                    Some(plan.plan.manifest_digest()),
                    Some(input.owner_task_id),
                ),
            )?;
            let integrity_update =
                sqlx::query("UPDATE managed_projection_integrity SET mac=? WHERE managed_mcp_id=?")
                    .bind(projection_mac)
                    .bind(&managed_mcp_id)
                    .execute(&mut **tx)
                    .await
                    .map_err(map_sqlx)?;
            if integrity_update.rows_affected() != 1 {
                return Err(integrity_error());
            }
            tx.commit().await.map_err(map_sqlx)?;
            return self.get_connection_projection(&managed_mcp_id).await;
        }
        sqlx::query(r#"INSERT INTO projection_writer_bindings(managed_mcp_id,task_id,plan_id,plan_digest,link_key,manifest_digest,projection_digest,lifecycle_acquired_at_ms,worker_owner_id,worker_lease_expires_at_ms,step_ordinal,step_token,mac) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)"#)
            .bind(&managed_mcp_id).bind(input.owner_task_id).bind(input.plan_id).bind(&owner_task.plan_digest).bind(&link_key).bind(plan.plan.manifest_digest()).bind(&projection_digest).bind(lifecycle_acquired_at_ms).bind(input.worker_owner_id).bind(worker_lease_expires_at_ms).bind(input.step_ordinal).bind(input.step_token).bind(writer_mac)
            .execute(&mut **tx).await.map_err(map_sqlx)?;
        sqlx::query(
            r#"INSERT INTO connection_projections (
                managed_mcp_id, link_key, projection_json, revision, updated_at_ms,
                plan_id, manifest_digest, owner_task_id, projection_digest
            ) VALUES (?, ?, ?, 0, ?, ?, ?, ?, ?)"#,
        )
        .bind(&managed_mcp_id)
        .bind(&link_key)
        .bind(&projection_json)
        .bind(input.now_ms)
        .bind(input.plan_id)
        .bind(plan.plan.manifest_digest())
        .bind(input.owner_task_id)
        .bind(&projection_digest)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let projection_mac = self.integrity_signer.sign(
            "managed-projection",
            &managed_projection_integrity_payload(
                &managed_mcp_id,
                &link_key,
                &projection_digest,
                0,
                Some(input.plan_id),
                Some(plan.plan.manifest_digest()),
                Some(input.owner_task_id),
            ),
        )?;
        sqlx::query("INSERT INTO managed_projection_integrity(managed_mcp_id,mac) VALUES (?,?)")
            .bind(&managed_mcp_id)
            .bind(projection_mac)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_connection_projection(&managed_mcp_id).await
    }

    pub(crate) async fn remove_owned_projection(
        &self,
        input: RemoveOwnedProjection<'_>,
    ) -> McpPlatformResult<bool> {
        let mut tx = self.begin_immediate().await?;
        let task = fetch_task(&mut tx, input.task_id).await?;
        if task.status != TaskStatus::RollingBack
            || task.owner_id.as_deref() != Some(input.worker_owner_id)
            || task
                .lease_expires_at_ms
                .is_none_or(|expires_at_ms| expires_at_ms <= input.now_ms)
        {
            return Err(integrity_error());
        }
        let lifecycle_owner = sqlx::query_scalar::<_, String>(
            "SELECT task_id FROM managed_lifecycle_leases WHERE managed_mcp_id=? AND expires_at_ms>?",
        )
        .bind(input.managed_mcp_id)
        .bind(input.now_ms)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(integrity_error)?;
        if lifecycle_owner != input.task_id {
            return Err(integrity_error());
        }
        let compensation_step = fetch_task_step(
            &mut tx,
            self.integrity_signer.as_ref(),
            input.task_id,
            input.compensation_ordinal,
        )
        .await?;
        let expected_link_key = match &compensation_step.compensation {
            crate::mcp_platform::task::CompensationDescriptor::RemoveOwnedConnectionProjection {
                managed_mcp_id,
                link_key,
            } if managed_mcp_id == input.managed_mcp_id => link_key,
            _ => return Err(integrity_error()),
        };
        if compensation_step.compensation_status
            != crate::mcp_platform::task::CompensationStatus::Started
        {
            return Err(integrity_error());
        }
        let projection = sqlx::query(
            "SELECT link_key,plan_id,manifest_digest,projection_digest,owner_task_id FROM connection_projections WHERE managed_mcp_id=?",
        )
        .bind(input.managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let Some(projection) = projection else {
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(false);
        };
        if projection
            .try_get::<Option<String>, _>("owner_task_id")
            .map_err(map_sqlx)?
            .as_deref()
            != Some(input.task_id)
        {
            return Err(integrity_error());
        }
        verify_projection_writer_binding(
            &mut tx,
            self.integrity_signer.as_ref(),
            input.managed_mcp_id,
            input.task_id,
            &projection
                .try_get::<String, _>("plan_id")
                .map_err(map_sqlx)?,
            &projection
                .try_get::<String, _>("link_key")
                .map_err(map_sqlx)?,
            &projection
                .try_get::<String, _>("manifest_digest")
                .map_err(map_sqlx)?,
            &projection
                .try_get::<String, _>("projection_digest")
                .map_err(map_sqlx)?,
        )
        .await?;
        if projection
            .try_get::<String, _>("link_key")
            .map_err(map_sqlx)?
            != *expected_link_key
        {
            return Err(integrity_error());
        }
        let integrity_delete =
            sqlx::query("DELETE FROM managed_projection_integrity WHERE managed_mcp_id=?")
                .bind(input.managed_mcp_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
        if integrity_delete.rows_affected() != 1 {
            return Err(integrity_error());
        }
        sqlx::query("DELETE FROM projection_writer_bindings WHERE managed_mcp_id=? AND task_id=?")
            .bind(input.managed_mcp_id)
            .bind(input.task_id)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        let result = sqlx::query(
            "DELETE FROM connection_projections WHERE managed_mcp_id = ? AND owner_task_id = ?",
        )
        .bind(input.managed_mcp_id)
        .bind(input.task_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn restore_owned_connection_projection(
        &self,
        input: RestoreOwnedProjection<'_>,
    ) -> McpPlatformResult<ConnectionProjectionRecord> {
        let mut tx = self.begin_immediate().await?;
        let validation =
            validate_owned_projection_restore(&mut tx, self.integrity_signer.as_ref(), &input)
                .await?;
        if validation.already_restored {
            tx.commit().await.map_err(map_sqlx)?;
            return self.get_connection_projection(input.managed_mcp_id).await;
        }
        let result = sqlx::query(r#"UPDATE connection_projections SET projection_json = ?,revision = revision + 1,updated_at_ms = ?,plan_id = ?,manifest_digest = ?,owner_task_id = ?,projection_digest = ? WHERE managed_mcp_id = ? AND link_key = ? AND owner_task_id = ?"#)
            .bind(validation.projection_json).bind(input.now_ms).bind(input.plan_id).bind(input.manifest_digest).bind(input.owner_task_id).bind(input.projection_digest).bind(input.managed_mcp_id).bind(input.link_key).bind(input.replacing_task_id)
            .execute(&mut **tx).await.map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            return Err(integrity_error());
        }
        let projection_mac = self.integrity_signer.sign(
            "managed-projection",
            &managed_projection_integrity_payload(
                input.managed_mcp_id,
                input.link_key,
                input.projection_digest,
                validation.current_revision + 1,
                Some(input.plan_id),
                Some(input.manifest_digest),
                input.owner_task_id,
            ),
        )?;
        let integrity_update =
            sqlx::query("UPDATE managed_projection_integrity SET mac=? WHERE managed_mcp_id=?")
                .bind(projection_mac)
                .bind(input.managed_mcp_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
        if integrity_update.rows_affected() != 1 {
            return Err(integrity_error());
        }
        tx.commit().await.map_err(map_sqlx)?;
        self.get_connection_projection(input.managed_mcp_id).await
    }

    pub async fn validate_owned_connection_projection_restore(
        &self,
        input: RestoreOwnedProjection<'_>,
    ) -> McpPlatformResult<()> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx)?;
        validate_owned_projection_restore(&mut tx, self.integrity_signer.as_ref(), &input).await?;
        tx.rollback().await.map_err(map_sqlx)
    }

    pub(crate) async fn remove_owned_managed_mcp(
        &self,
        managed_mcp_id: &str,
        owner_task_id: &str,
    ) -> McpPlatformResult<bool> {
        let mut tx = self.begin_immediate().await?;
        let projection_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM connection_projections WHERE managed_mcp_id = ?",
        )
        .bind(managed_mcp_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if projection_count != 0 {
            return Ok(false);
        }
        sqlx::query(
            "DELETE FROM managed_versions WHERE managed_mcp_id = ? AND EXISTS (SELECT 1 FROM managed_mcps WHERE managed_mcp_id = ? AND owner_task_id = ?)",
        )
        .bind(managed_mcp_id)
        .bind(managed_mcp_id)
        .bind(owner_task_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let result =
            sqlx::query("DELETE FROM managed_mcps WHERE managed_mcp_id = ? AND owner_task_id = ?")
                .bind(managed_mcp_id)
                .bind(owner_task_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn create_health_task(
        &self,
        input: CreateHealthTask<'_>,
    ) -> McpPlatformResult<TaskRecord> {
        let mut tx = self.begin_immediate().await?;
        if let Some(row) =
            sqlx::query("SELECT * FROM tasks WHERE operation = 'health' AND idempotency_key = ?")
                .bind(input.idempotency_key)
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx)?
        {
            let task = decode_task_row(&row)?;
            let request = sqlx::query(
                "SELECT managed_mcp_id, mode FROM health_task_requests WHERE task_id = ?",
            )
            .bind(&task.task_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(map_sqlx)?;
            let same = request
                .try_get::<String, _>("managed_mcp_id")
                .map_err(map_sqlx)?
                == input.managed_mcp_id
                && request.try_get::<String, _>("mode").map_err(map_sqlx)? == input.mode.as_str();
            tx.commit().await.map_err(map_sqlx)?;
            return if same {
                Ok(task)
            } else {
                Err(error(
                    McpPlatformErrorCode::IdempotencyConflict,
                    "health idempotency key already refers to different content",
                ))
            };
        }
        let projection = sqlx::query(
            r#"SELECT cp.plan_id,ip.plan_digest,cp.link_key,cp.projection_digest,
                       cp.revision AS projection_revision,cp.manifest_digest,
                       cp.owner_task_id,mv.adapter_evidence_json,mi.mac,
                       mm.active_manifest_digest,mv.manifest_digest AS version_manifest_digest
                FROM connection_projections cp
                JOIN install_plans ip ON ip.plan_id = cp.plan_id
                JOIN managed_mcps mm ON mm.managed_mcp_id = cp.managed_mcp_id
                JOIN managed_versions mv ON mv.managed_mcp_id = mm.managed_mcp_id AND mv.active = 1
                JOIN managed_projection_integrity mi ON mi.managed_mcp_id=cp.managed_mcp_id
                WHERE cp.managed_mcp_id = ?"#,
        )
        .bind(input.managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let plan_id: String = projection.try_get("plan_id").map_err(map_sqlx)?;
        let plan_digest: String = projection.try_get("plan_digest").map_err(map_sqlx)?;
        let link_key: String = projection.try_get("link_key").map_err(map_sqlx)?;
        let projection_digest: String =
            projection.try_get("projection_digest").map_err(map_sqlx)?;
        let projection_revision: i64 = projection
            .try_get("projection_revision")
            .map_err(map_sqlx)?;
        let manifest_digest: String = projection.try_get("manifest_digest").map_err(map_sqlx)?;
        let owner_task_id: String = projection.try_get("owner_task_id").map_err(map_sqlx)?;
        if projection
            .try_get::<Option<String>, _>("active_manifest_digest")
            .map_err(map_sqlx)?
            .as_deref()
            != Some(&manifest_digest)
            || projection
                .try_get::<String, _>("version_manifest_digest")
                .map_err(map_sqlx)?
                != manifest_digest
        {
            return Err(integrity_error());
        }
        self.integrity_signer.verify(
            "managed-projection",
            &managed_projection_integrity_payload(
                input.managed_mcp_id,
                &link_key,
                &projection_digest,
                projection_revision,
                Some(&plan_id),
                Some(&manifest_digest),
                Some(&owner_task_id),
            ),
            &projection.try_get::<String, _>("mac").map_err(map_sqlx)?,
        )?;
        verify_projection_writer_binding(
            &mut tx,
            self.integrity_signer.as_ref(),
            input.managed_mcp_id,
            &owner_task_id,
            &plan_id,
            &link_key,
            &manifest_digest,
            &projection_digest,
        )
        .await?;
        sqlx::query(
            r#"INSERT INTO tasks (
                task_id, plan_id, plan_digest, operation, idempotency_key, status, actor,
                created_at_ms, updated_at_ms, progress, step_cursor, adapter_evidence_json,
                rollback_status, revision, event_sequence
            ) VALUES (?, ?, ?, 'health', ?, 'queued', ?, ?, ?, 0, 0, ?, 'not_required', 0, 1)"#,
        )
        .bind(input.task_id)
        .bind(&plan_id)
        .bind(&plan_digest)
        .bind(input.idempotency_key)
        .bind(input.actor)
        .bind(input.now_ms)
        .bind(input.now_ms)
        .bind(
            projection
                .try_get::<Option<String>, _>("adapter_evidence_json")
                .map_err(map_sqlx)?,
        )
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "INSERT INTO health_task_requests(task_id, managed_mcp_id, mode) VALUES (?, ?, ?)",
        )
        .bind(input.task_id)
        .bind(input.managed_mcp_id)
        .bind(input.mode.as_str())
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        append_audit(
            &mut tx,
            NewAuditEvent {
                task_id: input.task_id,
                sequence: 1,
                event_type: AuditEventType::TaskCreated,
                actor: input.actor,
                occurred_at_ms: input.now_ms,
                payload: &AuditPayload::TaskCreated {
                    status: TaskStatus::Queued,
                },
                redacted_error: None,
            },
        )
        .await?;
        let task = fetch_task(&mut tx, input.task_id).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(task)
    }

    pub async fn get_health_task_request(
        &self,
        task_id: &str,
    ) -> McpPlatformResult<HealthTaskRequestRecord> {
        let row = sqlx::query(
            "SELECT task_id, managed_mcp_id, mode FROM health_task_requests WHERE task_id = ?",
        )
        .bind(task_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        Ok(HealthTaskRequestRecord {
            task_id: row.try_get("task_id").map_err(map_sqlx)?,
            managed_mcp_id: row.try_get("managed_mcp_id").map_err(map_sqlx)?,
            mode: decode_database_enum(&row.try_get::<String, _>("mode").map_err(map_sqlx)?)?,
        })
    }

    pub(crate) async fn append_health_observation(
        &self,
        input: NewHealthObservation<'_>,
    ) -> McpPlatformResult<HealthObservationRecord> {
        if input.latency_ms < 0 {
            return Err(integrity_error());
        }
        let mut tx = self.begin_immediate().await?;
        if let Some(observation_id) = sqlx::query_scalar::<_, i64>(
            "SELECT observation_id FROM health_observations WHERE task_id = ?",
        )
        .bind(input.task_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        {
            tx.commit().await.map_err(map_sqlx)?;
            return self.get_health_observation(observation_id).await;
        }
        let state_json = sqlx::query_scalar::<_, String>(
            "SELECT state_json FROM managed_mcps WHERE managed_mcp_id = ?",
        )
        .bind(input.managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let mut state: ManagedMcpState = decode(&state_json)?;
        state.health = input.result_code.health_state();
        let result = sqlx::query(
            r#"INSERT INTO health_observations (
                managed_mcp_id, task_id, check_type, result_code, latency_ms,
                capabilities_digest, tools_digest, checked_at_ms, detail_json
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(input.managed_mcp_id)
        .bind(input.task_id)
        .bind(input.check_type)
        .bind(input.result_code.as_str())
        .bind(input.latency_ms)
        .bind(input.capabilities_digest)
        .bind(input.tools_digest)
        .bind(input.checked_at_ms)
        .bind(encode(&input.detail_code)?)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE managed_mcps SET state_json = ?, revision = revision + 1, updated_at_ms = ? WHERE managed_mcp_id = ?",
        )
        .bind(encode(&state)?)
        .bind(input.checked_at_ms)
        .bind(input.managed_mcp_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let observation_id = result.last_insert_rowid();
        tx.commit().await.map_err(map_sqlx)?;
        self.get_health_observation(observation_id).await
    }

    pub async fn get_health_observation(
        &self,
        observation_id: i64,
    ) -> McpPlatformResult<HealthObservationRecord> {
        let row = sqlx::query("SELECT * FROM health_observations WHERE observation_id = ?")
            .bind(observation_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        decode_health_observation(&row)
    }

    pub async fn latest_health_observation(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<HealthObservationRecord>> {
        let row = sqlx::query(
            "SELECT * FROM health_observations WHERE managed_mcp_id = ? ORDER BY checked_at_ms DESC, observation_id DESC LIMIT 1",
        )
        .bind(managed_mcp_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        row.as_ref().map(decode_health_observation).transpose()
    }

    pub async fn list_retry_attempts(
        &self,
        task_id: &str,
    ) -> McpPlatformResult<Vec<RetryAttemptRecord>> {
        let rows =
            sqlx::query("SELECT * FROM task_retry_attempts WHERE task_id = ? ORDER BY attempt")
                .bind(task_id)
                .fetch_all(&self.pool)
                .await
                .map_err(map_sqlx)?;
        rows.iter()
            .map(|row| {
                Ok(RetryAttemptRecord {
                    task_id: row.try_get("task_id").map_err(map_sqlx)?,
                    attempt: row.try_get("attempt").map_err(map_sqlx)?,
                    idempotency_key: row.try_get("idempotency_key").map_err(map_sqlx)?,
                    requested_from_status: decode_database_enum(
                        &row.try_get::<String, _>("requested_from_status")
                            .map_err(map_sqlx)?,
                    )?,
                    actor: row.try_get("actor").map_err(map_sqlx)?,
                    created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
                })
            })
            .collect()
    }

    pub(in crate::mcp_platform) async fn begin_projection_mutation_authorized(
        &self,
        _capability: &ProjectionWriteCapability,
        authority: &RuntimeProjectionAuthority,
        managed_mcp_id: &str,
        expected_revision: i64,
        desired_enabled: bool,
        now_ms: i64,
    ) -> McpPlatformResult<ProjectionMutationRecord> {
        let mut tx = self.begin_immediate().await?;
        let lifecycle_busy = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM managed_lifecycle_leases WHERE managed_mcp_id = ?)",
        )
        .bind(managed_mcp_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if lifecycle_busy {
            return Err(error(
                McpPlatformErrorCode::PlanConflict,
                "managed MCP lifecycle mutation is active",
            ));
        }
        if let Some(row) = sqlx::query(
            "SELECT * FROM projection_mutations WHERE managed_mcp_id = ? AND status IN ('started','config_committed','recovery_required')",
        )
        .bind(managed_mcp_id)
        .fetch_optional(&mut **tx)
            .await
            .map_err(map_sqlx)?
        {
            let record = decode_projection_mutation(&row)?;
            validate_projection_mutation_witness_record(&record)?;
            tx.rollback().await?;
            return if record.status != ProjectionMutationStatus::RecoveryRequired
                && record.expected_revision == expected_revision
                && record.desired_enabled == desired_enabled
            {
                Ok(record)
            } else if record.status == ProjectionMutationStatus::RecoveryRequired {
                Err(error(
                    McpPlatformErrorCode::ProjectionConflict,
                    "managed MCP projection recovery must be resolved before changing enablement",
                ))
            } else {
                Err(error(
                    McpPlatformErrorCode::RevisionConflict,
                    "managed MCP has an active projection mutation",
                ))
            };
        }
        let row =
            sqlx::query("SELECT revision, state_json FROM managed_mcps WHERE managed_mcp_id = ?")
                .bind(managed_mcp_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx)?
                .ok_or_else(not_found)?;
        let revision: i64 = row.try_get("revision").map_err(map_sqlx)?;
        if revision != expected_revision {
            return Err(error(
                McpPlatformErrorCode::RevisionConflict,
                "managed MCP revision changed",
            ));
        }
        let state: ManagedMcpState =
            decode(&row.try_get::<String, _>("state_json").map_err(map_sqlx)?)?;
        let result = sqlx::query("INSERT INTO projection_mutations(managed_mcp_id, expected_revision, previous_enabled, desired_enabled, status, created_at_ms, updated_at_ms) VALUES (?, ?, ?, ?, 'started', ?, ?)")
            .bind(managed_mcp_id).bind(expected_revision).bind(state.default_enabled).bind(desired_enabled).bind(now_ms).bind(now_ms)
            .execute(&mut **tx).await.map_err(map_sqlx)?;
        let pending = ProjectionMutationRecord {
            mutation_id: result.last_insert_rowid(),
            managed_mcp_id: managed_mcp_id.to_string(),
            expected_revision,
            previous_enabled: state.default_enabled,
            desired_enabled,
            status: ProjectionMutationStatus::Started,
            writer_runtime_id: None,
            writer_sink_id: None,
            writer_anchor: None,
            writer_witness_version: None,
            writer_provider_id: None,
            writer_binding_domain: None,
            writer_observed_state_digest: None,
            writer_commitment: None,
        };
        let projection = self
            .fetch_connection_projection_snapshot(&mut tx, managed_mcp_id)
            .await?;
        let mut writer = authority.bind_mutation_with_repository_identity(
            projection_repository_identity(&tx),
            &pending,
            projection.as_ref(),
        )?;
        let unsigned_record = ProjectionMutationRecord {
            writer_runtime_id: Some(writer.witness.runtime_id.clone()),
            writer_sink_id: Some(writer.witness.sink_identity.clone()),
            writer_anchor: Some(writer.witness.projection_anchor()),
            writer_witness_version: Some(writer.witness.version),
            writer_provider_id: Some(writer.witness.repository.provider_id.clone()),
            writer_binding_domain: Some(writer.witness.domain.clone()),
            writer_observed_state_digest: Some(writer.witness.observed_state_digest.clone()),
            writer_commitment: Some("0".repeat(64)),
            ..pending.clone()
        };
        writer.commitment = self.integrity_signer.sign(
            "projection-mutation-authorization-v2",
            &authority.mutation_commitment_payload(&unsigned_record)?,
        )?;
        sqlx::query(
            r#"UPDATE projection_mutations
               SET writer_runtime_id=?,writer_sink_id=?,writer_anchor_instance_id=?,
                   writer_anchor_path_binding=?,writer_anchor_key_epoch=?,
                   writer_anchor_sequence=?,writer_anchor_root=?,writer_witness_version=?,
                   writer_provider_id=?,writer_binding_domain=?,writer_observed_state_digest=?,
                   writer_commitment=?
               WHERE mutation_id=?"#,
        )
        .bind(&writer.witness.runtime_id)
        .bind(&writer.witness.sink_identity)
        .bind(&writer.witness.repository.instance_id)
        .bind(&writer.witness.repository.path_binding)
        .bind(i64::try_from(writer.witness.repository.key_epoch).map_err(|_| integrity_error())?)
        .bind(1_i64)
        .bind(&writer.witness.projection_digest)
        .bind(writer.witness.version)
        .bind(&writer.witness.repository.provider_id)
        .bind(&writer.witness.domain)
        .bind(&writer.witness.observed_state_digest)
        .bind(&writer.commitment)
        .bind(pending.mutation_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let record = ProjectionMutationRecord {
            writer_runtime_id: Some(writer.witness.runtime_id.clone()),
            writer_sink_id: Some(writer.witness.sink_identity.clone()),
            writer_anchor: Some(writer.witness.projection_anchor()),
            writer_witness_version: Some(writer.witness.version),
            writer_provider_id: Some(writer.witness.repository.provider_id),
            writer_binding_domain: Some(writer.witness.domain),
            writer_observed_state_digest: Some(writer.witness.observed_state_digest),
            writer_commitment: Some(writer.commitment),
            ..pending
        };
        tx.commit().await.map_err(map_sqlx)?;
        Ok(record)
    }

    pub(in crate::mcp_platform) async fn authorize_projection_mutation(
        &self,
        _capability: &ProjectionWriteCapability,
        authority: &RuntimeProjectionAuthority,
        mutation_id: i64,
    ) -> McpPlatformResult<ProjectionAuthorization> {
        let mut tx = self.begin_immediate().await?;
        let row = sqlx::query("SELECT * FROM projection_mutations WHERE mutation_id=?")
            .bind(mutation_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        let mutation = decode_projection_mutation(&row)?;
        if !matches!(
            mutation.status,
            ProjectionMutationStatus::Started
                | ProjectionMutationStatus::ConfigCommitted
                | ProjectionMutationStatus::RecoveryRequired
        ) {
            return Err(projection_mutation_status_error(mutation.status));
        }
        validate_projection_mutation_witness_record(&mutation)?;
        if !authority.validates_mutation_binding_with_repository_identity(
            &mutation,
            &projection_repository_identity(&tx),
        ) {
            return Err(projection_witness_expired());
        }
        self.integrity_signer.verify(
            "projection-mutation-authorization-v2",
            &authority.mutation_commitment_payload(&mutation)?,
            mutation
                .writer_commitment
                .as_deref()
                .ok_or_else(integrity_error)?,
        )?;
        let current_anchor = super::projection_authority_anchor(&tx.checkpoint);
        let inventory =
            Self::fetch_managed_inventory_snapshot(&mut tx, &mutation.managed_mcp_id).await?;
        if inventory.managed.revision != mutation.expected_revision
            || inventory.managed.state.default_enabled != mutation.previous_enabled
        {
            return Err(projection_witness_expired());
        }
        let projection = self
            .fetch_connection_projection_snapshot(&mut tx, &mutation.managed_mcp_id)
            .await?;
        let (plan, manifest) = match projection.as_ref() {
            Some(projection) => {
                if projection.manifest_digest.as_deref()
                    != inventory.lifecycle.active_manifest_digest.as_deref()
                    || projection.owner_task_id.is_none()
                {
                    return Err(projection_witness_expired());
                }
                let plan = fetch_valid_plan(
                    &mut tx,
                    projection.plan_id.as_deref().ok_or_else(integrity_error)?,
                )
                .await?;
                if plan.policy_evidence.is_denied() {
                    return Err(error(
                        McpPlatformErrorCode::PolicyDenied,
                        "projection mutation policy authority was revoked",
                    ));
                }
                let (manifest, _) = self.resolve_manifest_for_plan(&plan).await?;
                (Some(plan), Some(manifest))
            }
            None if !mutation.desired_enabled => (None, None),
            None => return Err(projection_witness_expired()),
        };
        let witness = mutation.witness_v2().ok_or_else(integrity_error)?;
        if witness.projection_digest != projection_binding_digest(projection.as_ref())
            || witness.runtime_id
                != projection.as_ref().map_or_else(
                    || {
                        mutation
                            .writer_runtime_id
                            .clone()
                            .ok_or_else(integrity_error)
                    },
                    |projection| Ok(projection.link_key.clone()),
                )?
            || witness.observed_state_digest
                != expected_observed_state_digest(
                    authority.sink_identity(),
                    &witness.runtime_id,
                    &mutation,
                    projection.as_ref(),
                )?
        {
            return Err(projection_witness_expired());
        }
        if mutation.desired_enabled
            && (inventory.managed.state.registration != RegistrationState::Registered
                || inventory.managed.state.health != crate::mcp_platform::HealthState::Healthy)
        {
            return Err(error(
                McpPlatformErrorCode::HealthFailed,
                "projection mutation no longer has healthy managed authority",
            ));
        }
        tx.rollback().await?;
        Ok(ProjectionAuthorization::new(
            current_anchor,
            mutation,
            inventory,
            projection,
            plan,
            manifest,
        ))
    }

    pub(in crate::mcp_platform) async fn mark_projection_config_committed_authorized(
        &self,
        _capability: &ProjectionWriteCapability,
        authority: &RuntimeProjectionAuthority,
        authorization: &ProjectionAuthorization,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        if !authority.validates_authorization(authorization).await {
            return Err(projection_witness_expired());
        }
        let mut tx = self.begin_immediate().await?;
        validate_projection_authorization_checkpoint(&tx, authorization)?;
        validate_projection_authorization_state(&mut tx, authorization, false).await?;
        let result = sqlx::query(
            r#"UPDATE projection_mutations SET status='config_committed',updated_at_ms=?
               WHERE mutation_id=? AND expected_revision=? AND status IN ('started','recovery_required')"#,
        )
        .bind(now_ms)
        .bind(authorization.mutation.mutation_id)
        .bind(authorization.mutation.expected_revision)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            let current =
                fetch_projection_mutation_status(&mut tx, authorization.mutation.mutation_id)
                    .await?;
            return Err(projection_mutation_status_error(current));
        }
        tx.commit().await
    }

    pub(in crate::mcp_platform) async fn complete_projection_mutation_authorized(
        &self,
        _capability: &ProjectionWriteCapability,
        authority: &RuntimeProjectionAuthority,
        authorization: &ProjectionAuthorization,
        receipt: &ProjectionSinkCommitReceipt,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        if !authority.validates_authorization(authorization).await
            || !authority.validates_receipt_identity(receipt).await
        {
            return Err(projection_witness_expired());
        }
        let witness = validate_projection_commit_receipt(authorization, receipt)?;
        if witness.projection_digest
            != projection_binding_digest(authorization.projection().map(|projection| projection))
            || witness.observed_state_digest
                != expected_observed_state_digest(
                    authority.sink_identity(),
                    &witness.runtime_id,
                    authorization.mutation(),
                    authorization.projection(),
                )?
        {
            return Err(integrity_error());
        }
        let mut tx = self.begin_immediate().await?;
        validate_projection_authorization_checkpoint(&tx, authorization)?;
        validate_projection_authorization_state(&mut tx, authorization, true).await?;
        self.integrity_signer.verify(
            PROJECTION_SINK_COMMIT_RECEIPT_DOMAIN,
            &projection_sink_receipt_binding_payload(
                PROJECTION_SINK_COMMIT_RECEIPT_DOMAIN,
                receipt.witness(),
                receipt.checkpoint(),
                receipt.proof(),
            ),
            receipt.repository_binding(),
        )?;
        let mut state = authorization.inventory.managed.state.clone();
        state.default_enabled = authorization.mutation.desired_enabled;
        let managed = sqlx::query(
            r#"UPDATE managed_mcps SET state_json=?,revision=revision+1,updated_at_ms=?
               WHERE managed_mcp_id=? AND revision=?"#,
        )
        .bind(encode(&state)?)
        .bind(now_ms)
        .bind(&authorization.mutation.managed_mcp_id)
        .bind(authorization.mutation.expected_revision)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let mutation = sqlx::query(
            r#"UPDATE projection_mutations SET status='committed',updated_at_ms=?
               WHERE mutation_id=? AND expected_revision=? AND status='config_committed'"#,
        )
        .bind(now_ms)
        .bind(authorization.mutation.mutation_id)
        .bind(authorization.mutation.expected_revision)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if managed.rows_affected() != 1 || mutation.rows_affected() != 1 {
            let current =
                fetch_projection_mutation_status(&mut tx, authorization.mutation.mutation_id)
                    .await?;
            return Err(if managed.rows_affected() != 1 {
                projection_witness_expired()
            } else {
                projection_mutation_status_error(current)
            });
        }
        tx.commit().await
    }

    pub async fn list_pending_projection_mutations(
        &self,
    ) -> McpPlatformResult<Vec<ProjectionMutationRecord>> {
        let mut tx = self.begin_immediate().await?;
        let rows = sqlx::query("SELECT * FROM projection_mutations WHERE status IN ('started','config_committed','recovery_required') ORDER BY mutation_id")
            .fetch_all(&mut **tx).await.map_err(map_sqlx)?;
        let mutations = rows
            .iter()
            .map(decode_projection_mutation)
            .map(|result| {
                let mutation = result?;
                validate_projection_mutation_witness_record(&mutation)?;
                Ok(mutation)
            })
            .collect::<McpPlatformResult<Vec<_>>>()?;
        tx.rollback().await?;
        Ok(mutations)
    }

    pub async fn projection_recovery_required(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<bool> {
        let mut tx = self.begin_verified_read_snapshot().await?;
        let required = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM projection_mutations WHERE managed_mcp_id = ? AND status = 'recovery_required')",
        )
        .bind(managed_mcp_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        tx.rollback().await.map_err(map_sqlx)?;
        Ok(required)
    }

    pub(in crate::mcp_platform) async fn mark_projection_mutation_recovery_required_authorized(
        &self,
        _capability: &ProjectionWriteCapability,
        authority: &RuntimeProjectionAuthority,
        mutation_id: i64,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let row = sqlx::query("SELECT * FROM projection_mutations WHERE mutation_id=?")
            .bind(mutation_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        let mutation = decode_projection_mutation(&row)?;
        validate_projection_mutation_witness_record(&mutation)?;
        if !authority.validates_mutation_binding_with_repository_identity(
            &mutation,
            &projection_repository_identity(&tx),
        ) {
            return Err(projection_witness_expired());
        }
        self.integrity_signer.verify(
            "projection-mutation-authorization-v2",
            &authority.mutation_commitment_payload(&mutation)?,
            mutation
                .writer_commitment
                .as_deref()
                .ok_or_else(integrity_error)?,
        )?;
        sqlx::query("UPDATE projection_mutations SET status = 'recovery_required', updated_at_ms = ? WHERE mutation_id = ? AND status IN ('started','config_committed')")
            .bind(now_ms).bind(mutation_id).execute(&mut **tx).await.map_err(map_sqlx)?;
        tx.commit().await?;
        Ok(())
    }

    pub(in crate::mcp_platform) async fn force_projection_mutation_recovery_required(
        &self,
        _capability: &ProjectionWriteCapability,
        mutation_id: i64,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let _ = self.integrity_signer.identity()?;
        let mut tx = self.begin_immediate().await?;
        sqlx::query(
            "UPDATE projection_mutations SET status = 'recovery_required', updated_at_ms = ? WHERE mutation_id = ? AND status IN ('started','config_committed','recovery_required')",
        )
        .bind(now_ms)
        .bind(mutation_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await?;
        Ok(())
    }

    pub(in crate::mcp_platform) async fn resolve_projection_mutation_recovery_authorized(
        &self,
        _capability: &ProjectionWriteCapability,
        authority: &RuntimeProjectionAuthority,
        authorization: &ProjectionAuthorization,
        receipt: &ProjectionRecoveryConfirmationReceipt,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        if !authority.validates_authorization(authorization).await
            || !authority
                .validates_recovery_confirmation_identity(receipt)
                .await
        {
            return Err(projection_witness_expired());
        }
        validate_projection_recovery_confirmation_receipt(authority, authorization, receipt)?;
        let mut tx = self.begin_immediate().await?;
        validate_projection_authorization_checkpoint(&tx, authorization)?;
        validate_projection_authorization_state(&mut tx, authorization, false).await?;
        self.integrity_signer.verify(
            PROJECTION_SINK_RECOVERY_RECEIPT_DOMAIN,
            &projection_sink_receipt_binding_payload(
                PROJECTION_SINK_RECOVERY_RECEIPT_DOMAIN,
                receipt.witness(),
                receipt.checkpoint(),
                receipt.proof(),
            ),
            receipt.repository_binding(),
        )?;
        let mutation_id = authorization.mutation().mutation_id;
        let row = sqlx::query("SELECT * FROM projection_mutations WHERE mutation_id=?")
            .bind(mutation_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        let mutation = decode_projection_mutation(&row)?;
        if mutation.status != ProjectionMutationStatus::RecoveryRequired {
            return Err(projection_mutation_status_error(mutation.status));
        }
        validate_projection_mutation_witness_record(&mutation)?;
        if !authority.validates_mutation_binding_with_repository_identity(
            &mutation,
            &projection_repository_identity(&tx),
        ) {
            return Err(projection_witness_expired());
        }
        self.integrity_signer.verify(
            "projection-mutation-authorization-v2",
            &authority.mutation_commitment_payload(&mutation)?,
            mutation
                .writer_commitment
                .as_deref()
                .ok_or_else(integrity_error)?,
        )?;
        let result = sqlx::query(
            "UPDATE projection_mutations SET status = 'resolved', updated_at_ms = ? WHERE mutation_id = ? AND status = 'recovery_required'",
        )
        .bind(now_ms)
        .bind(mutation_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            let current = fetch_projection_mutation_status(&mut tx, mutation_id).await?;
            return Err(projection_mutation_status_error(current));
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqlx::Executor;

    use super::super::{InMemoryIntegritySigner, SqliteMcpPlatformRepository};
    use crate::mcp_platform::error::McpPlatformErrorCode;

    #[tokio::test]
    async fn verified_read_snapshot_completes_while_another_connection_holds_a_writer_lock() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("verified-read-snapshot.db");
        let signer = InMemoryIntegritySigner::new_for_testing([0x51; 32]);
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let checkpoint = signer.checkpoint().unwrap();

        let mut writer = repository.pool.acquire().await.unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *writer)
            .await
            .unwrap();

        let inventory = tokio::time::timeout(
            Duration::from_millis(250),
            repository.get_managed_inventory("absent-managed-mcp"),
        )
        .await
        .expect("inventory read must not wait for the writer lock")
        .unwrap_err();
        assert_eq!(inventory.code(), McpPlatformErrorCode::NotFound);

        let recovery_required = tokio::time::timeout(
            Duration::from_millis(250),
            repository.projection_recovery_required("absent-managed-mcp"),
        )
        .await
        .expect("recovery read must not wait for the writer lock")
        .unwrap();
        assert!(!recovery_required);
        assert_eq!(signer.checkpoint().unwrap(), checkpoint);

        sqlx::query("ROLLBACK").execute(&mut *writer).await.unwrap();
        repository.verify_integrity().await.unwrap();
        assert_eq!(signer.checkpoint().unwrap(), checkpoint);
    }
}

fn same_supply_chain_authority(
    persisted: Option<&str>,
    observed: Option<&SupplyChainEvidence>,
) -> McpPlatformResult<bool> {
    match (persisted, observed) {
        (None, None) => Ok(true),
        (Some(value), Some(observed)) => {
            let persisted: SupplyChainEvidence = decode(value)?;
            Ok(persisted.same_authority(observed))
        }
        _ => Ok(false),
    }
}

fn enum_json<T: serde::Serialize>(value: T) -> McpPlatformResult<String> {
    serde_json::to_string(&value)
        .map(|value| value.trim_matches('"').to_string())
        .map_err(|_| integrity_error())
}

fn validate_projection_mutation_witness_record(
    mutation: &ProjectionMutationRecord,
) -> McpPlatformResult<()> {
    let witness = mutation.witness_v2().ok_or_else(integrity_error)?;
    if witness.repository.provider_id.is_empty()
        || witness.sink_identity.is_empty()
        || witness.runtime_id.is_empty()
        || witness.projection_digest.len() != 64
        || witness.observed_state_digest.len() != 64
        || mutation
            .writer_commitment
            .as_deref()
            .map_or(true, |commitment| commitment.len() != 64)
    {
        return Err(integrity_error());
    }
    Ok(())
}

fn projection_repository_identity(
    tx: &super::AnchoredTransaction<'_>,
) -> ProjectionRepositoryIdentity {
    ProjectionRepositoryIdentity {
        provider_id: tx.checkpoint.identity.provider_id.clone(),
        instance_id: tx.checkpoint.identity.instance_id.clone(),
        path_binding: tx.checkpoint.identity.path_binding.clone(),
        key_epoch: tx.checkpoint.identity.key_epoch,
    }
}

fn decode_projection_mutation(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<ProjectionMutationRecord> {
    Ok(ProjectionMutationRecord {
        mutation_id: row.try_get("mutation_id").map_err(map_sqlx)?,
        managed_mcp_id: row.try_get("managed_mcp_id").map_err(map_sqlx)?,
        expected_revision: row.try_get("expected_revision").map_err(map_sqlx)?,
        previous_enabled: row.try_get("previous_enabled").map_err(map_sqlx)?,
        desired_enabled: row.try_get("desired_enabled").map_err(map_sqlx)?,
        status: decode_database_enum(&row.try_get::<String, _>("status").map_err(map_sqlx)?)?,
        writer_runtime_id: row.try_get("writer_runtime_id").map_err(map_sqlx)?,
        writer_sink_id: row.try_get("writer_sink_id").map_err(map_sqlx)?,
        writer_anchor: decode_projection_authority_anchor(row)?,
        writer_witness_version: row.try_get("writer_witness_version").map_err(map_sqlx)?,
        writer_provider_id: row.try_get("writer_provider_id").map_err(map_sqlx)?,
        writer_binding_domain: row.try_get("writer_binding_domain").map_err(map_sqlx)?,
        writer_observed_state_digest: row
            .try_get("writer_observed_state_digest")
            .map_err(map_sqlx)?,
        writer_commitment: row.try_get("writer_commitment").map_err(map_sqlx)?,
    })
}

fn decode_projection_authority_anchor(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<Option<ProjectionAuthorityAnchor>> {
    let instance_id: Option<String> = row.try_get("writer_anchor_instance_id").map_err(map_sqlx)?;
    let path_binding: Option<String> = row
        .try_get("writer_anchor_path_binding")
        .map_err(map_sqlx)?;
    let key_epoch: Option<i64> = row.try_get("writer_anchor_key_epoch").map_err(map_sqlx)?;
    let sequence: Option<i64> = row.try_get("writer_anchor_sequence").map_err(map_sqlx)?;
    let root: Option<String> = row.try_get("writer_anchor_root").map_err(map_sqlx)?;
    match (instance_id, path_binding, key_epoch, sequence, root) {
        (None, None, None, None, None) => Ok(None),
        (Some(instance_id), Some(path_binding), Some(key_epoch), Some(sequence), Some(root)) => {
            Ok(Some(ProjectionAuthorityAnchor {
                instance_id,
                path_binding,
                key_epoch: u64::try_from(key_epoch).map_err(|_| integrity_error())?,
                sequence: u64::try_from(sequence).map_err(|_| integrity_error())?,
                root,
            }))
        }
        _ => Err(integrity_error()),
    }
}

async fn validate_projection_authorization_state(
    tx: &mut Transaction<'_, Sqlite>,
    authorization: &ProjectionAuthorization,
    require_config_committed: bool,
) -> McpPlatformResult<()> {
    let mutation_row = sqlx::query("SELECT * FROM projection_mutations WHERE mutation_id=?")
        .bind(authorization.mutation.mutation_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
    let mutation = decode_projection_mutation(&mutation_row)?;
    if mutation.managed_mcp_id != authorization.mutation.managed_mcp_id
        || mutation.expected_revision != authorization.mutation.expected_revision
        || mutation.previous_enabled != authorization.mutation.previous_enabled
        || mutation.desired_enabled != authorization.mutation.desired_enabled
        || mutation.writer_runtime_id != authorization.mutation.writer_runtime_id
        || mutation.writer_sink_id != authorization.mutation.writer_sink_id
        || mutation.writer_anchor != authorization.mutation.writer_anchor
        || mutation.writer_witness_version != authorization.mutation.writer_witness_version
        || mutation.writer_provider_id != authorization.mutation.writer_provider_id
        || mutation.writer_binding_domain != authorization.mutation.writer_binding_domain
        || mutation.writer_observed_state_digest
            != authorization.mutation.writer_observed_state_digest
        || mutation.writer_commitment != authorization.mutation.writer_commitment
    {
        return Err(projection_mutation_status_error(mutation.status));
    }
    if require_config_committed && mutation.status != ProjectionMutationStatus::ConfigCommitted {
        return Err(projection_mutation_status_error(mutation.status));
    }
    let managed = sqlx::query(
        r#"SELECT revision,state_json,active_manifest_digest,active_version,owner_task_id
           FROM managed_mcps WHERE managed_mcp_id=?"#,
    )
    .bind(&mutation.managed_mcp_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx)?
    .ok_or_else(not_found)?;
    let state: ManagedMcpState = decode(
        &managed
            .try_get::<String, _>("state_json")
            .map_err(map_sqlx)?,
    )?;
    if managed.try_get::<i64, _>("revision").map_err(map_sqlx)? != mutation.expected_revision
        || state != authorization.inventory.managed.state
        || managed
            .try_get::<Option<String>, _>("active_manifest_digest")
            .map_err(map_sqlx)?
            != authorization.inventory.lifecycle.active_manifest_digest
        || managed
            .try_get::<Option<String>, _>("active_version")
            .map_err(map_sqlx)?
            != authorization.inventory.lifecycle.active_version
        || managed
            .try_get::<Option<String>, _>("owner_task_id")
            .map_err(map_sqlx)?
            != authorization.inventory.lifecycle.owner_task_id
    {
        return Err(projection_witness_expired());
    }
    match authorization.projection.as_ref() {
        Some(expected) => {
            let projection = sqlx::query(
                r#"SELECT link_key,revision,plan_id,manifest_digest,owner_task_id,projection_digest
                   FROM connection_projections WHERE managed_mcp_id=?"#,
            )
            .bind(&mutation.managed_mcp_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
            if projection
                .try_get::<String, _>("link_key")
                .map_err(map_sqlx)?
                != expected.link_key
                || projection.try_get::<i64, _>("revision").map_err(map_sqlx)? != expected.revision
                || projection
                    .try_get::<Option<String>, _>("plan_id")
                    .map_err(map_sqlx)?
                    != expected.plan_id
                || projection
                    .try_get::<Option<String>, _>("manifest_digest")
                    .map_err(map_sqlx)?
                    != expected.manifest_digest
                || projection
                    .try_get::<Option<String>, _>("owner_task_id")
                    .map_err(map_sqlx)?
                    != expected.owner_task_id
                || projection
                    .try_get::<String, _>("projection_digest")
                    .map_err(map_sqlx)?
                    != expected.projection_digest
            {
                return Err(projection_witness_expired());
            }
            let plan =
                fetch_valid_plan(tx, expected.plan_id.as_deref().ok_or_else(integrity_error)?)
                    .await?;
            let authorized_plan = authorization.plan.as_ref().ok_or_else(integrity_error)?;
            if plan.envelope_digest != authorized_plan.envelope_digest
                || plan.policy_evidence != authorized_plan.policy_evidence
                || plan.policy_evidence.is_denied()
            {
                return Err(projection_witness_expired());
            }
            let manifest = authorization
                .manifest
                .as_ref()
                .ok_or_else(integrity_error)?;
            if expected.manifest_digest.as_deref() != Some(manifest.verified.digest()) {
                return Err(projection_witness_expired());
            }
        }
        None => {
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM connection_projections WHERE managed_mcp_id=?)",
            )
            .bind(&mutation.managed_mcp_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(map_sqlx)?;
            if exists || mutation.desired_enabled {
                return Err(projection_witness_expired());
            }
        }
    }
    Ok(())
}

fn validate_projection_authorization_checkpoint(
    tx: &super::AnchoredTransaction<'_>,
    authorization: &ProjectionAuthorization,
) -> McpPlatformResult<()> {
    let witness = authorization.witness_v2()?;
    let current = super::projection_authority_anchor(&tx.checkpoint);
    if current.instance_id != authorization.checkpoint.instance_id
        || current.path_binding != authorization.checkpoint.path_binding
        || current.key_epoch != authorization.checkpoint.key_epoch
        || current.sequence != authorization.checkpoint.sequence
        || current.root != authorization.checkpoint.root
        || tx.checkpoint.identity.provider_id != witness.repository.provider_id
        || current.instance_id != witness.repository.instance_id
        || current.path_binding != witness.repository.path_binding
        || current.key_epoch != witness.repository.key_epoch
    {
        return Err(projection_witness_expired());
    }
    Ok(())
}

async fn fetch_projection_mutation_status(
    tx: &mut Transaction<'_, Sqlite>,
    mutation_id: i64,
) -> McpPlatformResult<ProjectionMutationStatus> {
    let row = sqlx::query("SELECT status FROM projection_mutations WHERE mutation_id = ?")
        .bind(mutation_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
    decode_database_enum(&row.try_get::<String, _>("status").map_err(map_sqlx)?)
}

const fn projection_witness_expired() -> crate::mcp_platform::error::McpPlatformError {
    error(
        McpPlatformErrorCode::ProjectionWitnessExpired,
        "stored projection witness no longer matches current authority",
    )
}

fn validate_projection_commit_receipt(
    authorization: &ProjectionAuthorization,
    receipt: &ProjectionSinkCommitReceipt,
) -> McpPlatformResult<crate::mcp_platform::repository::ProjectionWitnessV2> {
    let witness = authorization.witness_v2()?;
    if receipt.witness() != &witness || receipt.checkpoint() != authorization.checkpoint() {
        return Err(integrity_error());
    }
    if receipt.proof().runtime_id() != witness.runtime_id
        || receipt.proof().observed_state_digest() != witness.observed_state_digest
        || receipt.proof().target_state_digest() != witness.observed_state_digest
    {
        return Err(integrity_error());
    }
    if receipt.proof().kind() != ProjectionSinkAtomicProofKind::NoopCompareAndSwap
        && receipt.proof().kind() != ProjectionSinkAtomicProofKind::CompareAndSwapWrite
    {
        return Err(integrity_error());
    }
    Ok(witness)
}

fn validate_projection_recovery_confirmation_receipt(
    authority: &RuntimeProjectionAuthority,
    authorization: &ProjectionAuthorization,
    receipt: &ProjectionRecoveryConfirmationReceipt,
) -> McpPlatformResult<()> {
    let witness = authorization.witness_v2()?;
    if receipt.witness() != &witness || receipt.checkpoint() != authorization.checkpoint() {
        return Err(integrity_error());
    }
    let persisted_state_digest = authoritative_persisted_state_digest(
        authority.sink_identity(),
        &witness.runtime_id,
        authorization.inventory().managed.state.default_enabled,
        authorization.projection(),
    )?;
    if receipt.proof().kind() != ProjectionSinkAtomicProofKind::NoopCompareAndSwap
        || receipt.proof().runtime_id() != witness.runtime_id
        || receipt.proof().observed_state_digest() != persisted_state_digest
        || receipt.proof().target_state_digest() != persisted_state_digest
    {
        return Err(integrity_error());
    }
    Ok(())
}

const fn projection_witness_consumed() -> crate::mcp_platform::error::McpPlatformError {
    error(
        McpPlatformErrorCode::ProjectionWitnessConsumed,
        "stored projection witness was already consumed",
    )
}

const fn projection_mutation_status_error(
    status: ProjectionMutationStatus,
) -> crate::mcp_platform::error::McpPlatformError {
    match status {
        ProjectionMutationStatus::Committed | ProjectionMutationStatus::Resolved => {
            projection_witness_consumed()
        }
        ProjectionMutationStatus::Started
        | ProjectionMutationStatus::ConfigCommitted
        | ProjectionMutationStatus::RecoveryRequired => projection_witness_expired(),
    }
}

fn decode_health_observation(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<HealthObservationRecord> {
    Ok(HealthObservationRecord {
        observation_id: row.try_get("observation_id").map_err(map_sqlx)?,
        managed_mcp_id: row.try_get("managed_mcp_id").map_err(map_sqlx)?,
        task_id: row.try_get("task_id").map_err(map_sqlx)?,
        check_type: row.try_get("check_type").map_err(map_sqlx)?,
        result_code: decode_database_enum(
            &row.try_get::<String, _>("result_code").map_err(map_sqlx)?,
        )?,
        latency_ms: row.try_get("latency_ms").map_err(map_sqlx)?,
        capabilities_digest: row.try_get("capabilities_digest").map_err(map_sqlx)?,
        tools_digest: row.try_get("tools_digest").map_err(map_sqlx)?,
        checked_at_ms: row.try_get("checked_at_ms").map_err(map_sqlx)?,
        detail_code: decode(&row.try_get::<String, _>("detail_json").map_err(map_sqlx)?)?,
    })
}
