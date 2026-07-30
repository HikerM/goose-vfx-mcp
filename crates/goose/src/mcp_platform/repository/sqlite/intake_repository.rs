use rand::RngExt;
use sqlx::Row;

use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::intake::{
    format_minted_intake_private_reference, parse_minted_intake_private_reference,
    AuthorityCommitOutcome, AuthorityVerifiedOpaqueTuple, IntakeCandidateRecord,
    IntakeConfigurationDescriptor, IntakeGateReason, IntakeLifecycleState,
    IntakePrivateReferenceKind, IntakeSourceFacet, IntakeTransport, RecordIntakeApprovalGrant,
    RecordIntakeBindingReady, RecordIntakeConsentGrant, SaveIntakeCandidate,
    SaveIntakeConfigurationRef, VerifiedNoLiveProof,
};
use crate::mcp_platform::intake_inspection::{
    build_remote_blocked_snapshot, build_stdio_local_snapshot, classify_failure,
    trusted_policy_revision_for, validate_candidate_for_batch1_inspection, InspectionBindingTuple,
    InspectionConflictCode, InspectionConsentRecord, InspectionFactCode, InspectionFailureFamily,
    InspectionLifecycle, InspectionPurpose, InspectionSnapshot, InspectionState,
    InspectionSummaryCode, ObservationSurface, OperationPhase, RecordIntakeInspectionSnapshot,
    SaveIntakeInspectionConsent, StoredInspectionSnapshot,
};
use crate::mcp_platform::intake_remote_inspection::{
    advance_remote_inspection_attempt as advance_remote_inspection_attempt_state,
    claim_remote_inspection_attempt as build_remote_inspection_attempt, commit_state_for_outcome,
    parse_v25_persistence_purpose, recover_remote_inspection_attempt_stale_claim,
    v25_persistence_purpose_for_remote_inspection, validate_remote_inspection_consent_record,
    validate_remote_inspection_reservation, validate_remote_inspection_snapshot,
    RemoteInspectionAttemptRecord, RemoteInspectionAttemptState, RemoteInspectionAuthorityProvider,
    RemoteInspectionCommitState, RemoteInspectionConsentRecord, RemoteInspectionConsumptionRecord,
    RemoteInspectionGateError, RemoteInspectionNoLiveProofKind, RemoteInspectionPurpose,
    RemoteInspectionReservationEventType, RemoteInspectionReservationRecord,
    RemoteInspectionReservationState, RemoteInspectionSafeSubcode, RemoteInspectionSafeSummary,
    RemoteInspectionSnapshotRecord, REMOTE_INSPECTION_TRANSPORT_UNSUPPORTED_SUBCODE,
};
use crate::mcp_platform::service::port::{
    BindRemoteInspectionPendingCommit, ClaimCommittedRemoteInspectionAttempt,
    FinalizeRemoteInspectionConsumption, RecordRemoteInspectionCommitOutcome,
    RemoteInspectionRepositoryPort, ReserveOrLoadRemoteInspection,
    TombstoneRemoteInspectionPrebind,
};

use super::{decode, encode, map_sqlx, not_found, SqliteMcpPlatformRepository};

impl SqliteMcpPlatformRepository {
    pub(crate) fn mint_intake_private_reference(
        &self,
        kind: IntakePrivateReferenceKind,
    ) -> McpPlatformResult<String> {
        let mut nonce = [0_u8; 16];
        rand::rng().fill(&mut nonce);
        let nonce_hex = crate::utils::bytes_to_hex(nonce);
        let mac = self.integrity_signer.sign(
            "intake-private-reference",
            &intake_private_reference_mac_payload(kind, &nonce_hex),
        )?;
        Ok(format_minted_intake_private_reference(
            kind, &nonce_hex, &mac,
        ))
    }

    pub(crate) async fn save_intake_configuration_ref(
        &self,
        input: SaveIntakeConfigurationRef<'_>,
    ) -> McpPlatformResult<String> {
        let expected_source_facet = configuration_source_facet(input.descriptor);
        validate_private_reference_input(self, expected_source_facet, input.private_reference)?;
        let encoded = encode(input.descriptor)?;
        let mut tx = self.begin_immediate().await?;
        if let Some(existing) = sqlx::query(
            "SELECT configuration_ref,source_facet,private_reference FROM mcp_intake_configuration_refs WHERE descriptor_digest = ?",
        )
        .bind(input.descriptor_digest)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        {
            let stored_source_facet = IntakeSourceFacet::parse(
                &existing.try_get::<String, _>("source_facet").map_err(map_sqlx)?,
            )
            .ok_or_else(integrity_error)?;
            if stored_source_facet != expected_source_facet {
                tx.commit().await.map_err(map_sqlx)?;
                return Err(integrity_error());
            }
            validate_private_reference_state(
                self,
                stored_source_facet,
                existing
                    .try_get::<Option<String>, _>("private_reference")
                    .map_err(map_sqlx)?
                    .as_deref(),
            )?;
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(existing
                .try_get::<String, _>("configuration_ref")
                .map_err(map_sqlx)?);
        }
        sqlx::query(
            r#"INSERT INTO mcp_intake_configuration_refs(
                    configuration_ref,source_facet,redacted_descriptor_json,private_reference,
                    descriptor_digest,created_at_ms
               ) VALUES (?,?,?,?,?,?)"#,
        )
        .bind(input.configuration_ref)
        .bind(configuration_source_facet(input.descriptor).as_str())
        .bind(&encoded)
        .bind(input.private_reference)
        .bind(input.descriptor_digest)
        .bind(input.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(input.configuration_ref.to_string())
    }

    pub(crate) async fn save_intake_candidate(
        &self,
        input: SaveIntakeCandidate<'_>,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        validate_initial_candidate_state(input.source_facet, input.transport, input.initial_state)?;
        let mut tx = self.begin_immediate().await?;
        if let Some(existing) =
            load_candidate_by_submission_binding_tx(self, &mut tx, input.submission_binding).await?
        {
            if existing.source_facet == input.source_facet
                && existing.transport == input.transport
                && existing.descriptor_digest == input.descriptor_digest
            {
                tx.commit().await.map_err(map_sqlx)?;
                return Ok(existing);
            }
            tx.commit().await.map_err(map_sqlx)?;
            return Err(idempotency_conflict());
        }
        sqlx::query(
            r#"INSERT INTO mcp_intake_candidates(
                    candidate_id,submission_binding,source_facet,transport,redacted_configuration_ref,
                    descriptor_digest,created_at_ms
               ) VALUES (?,?,?,?,?,?,?)"#,
        )
        .bind(input.candidate_id)
        .bind(input.submission_binding)
        .bind(input.source_facet.as_str())
        .bind(input.transport.as_str())
        .bind(input.redacted_configuration_ref)
        .bind(input.descriptor_digest)
        .bind(input.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        append_candidate_revision_tx(
            &mut tx,
            input.candidate_id,
            1,
            input.initial_state,
            None,
            None,
            None,
            None,
            input.now_ms,
        )
        .await?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_intake_candidate(input.candidate_id).await
    }

    pub(crate) async fn get_intake_candidate(
        &self,
        candidate_id: &str,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        self.verify_integrity().await?;
        let row = sqlx::query(LATEST_CANDIDATE_SQL)
            .bind(candidate_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        decode_candidate_row(self, &row)
    }

    pub(crate) async fn get_intake_candidate_by_submission_binding(
        &self,
        submission_binding: &str,
    ) -> McpPlatformResult<Option<IntakeCandidateRecord>> {
        self.verify_integrity().await?;
        let row = sqlx::query(LATEST_CANDIDATE_BY_SUBMISSION_SQL)
            .bind(submission_binding)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?;
        row.as_ref()
            .map(|row| decode_candidate_row(self, row))
            .transpose()
    }

    pub(crate) async fn record_intake_consent_grant(
        &self,
        input: RecordIntakeConsentGrant<'_>,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        let mut tx = self.begin_immediate().await?;
        let current = load_candidate_by_id_tx(self, &mut tx, input.candidate_id)
            .await?
            .ok_or_else(not_found)?;
        if !matches!(
            current.lifecycle_state,
            IntakeLifecycleState::Submitted | IntakeLifecycleState::AwaitingConsent
        ) {
            append_conflict_revision_tx(
                &mut tx,
                &current,
                IntakeGateReason::CandidateStateConflict,
                input.now_ms,
            )
            .await?;
            tx.commit().await.map_err(map_sqlx)?;
            return Err(candidate_state_conflict());
        }
        let next_seq = current.revision + 1;
        append_candidate_revision_tx(
            &mut tx,
            input.candidate_id,
            next_seq,
            IntakeLifecycleState::ConsentGranted,
            None,
            current.approval_binding.as_deref(),
            current.manifest_identity_binding.as_deref(),
            current.registry_owned_payload_digest.as_deref(),
            input.now_ms,
        )
        .await?;
        sqlx::query(
            r#"INSERT INTO mcp_intake_consent_grants(
                    candidate_id,seq,consent_binding,created_at_ms
               ) VALUES (?,?,?,?)"#,
        )
        .bind(input.candidate_id)
        .bind(next_seq)
        .bind(input.consent_binding)
        .bind(input.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_intake_candidate(input.candidate_id).await
    }

    pub(crate) async fn record_intake_approval_grant(
        &self,
        input: RecordIntakeApprovalGrant<'_>,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        let mut tx = self.begin_immediate().await?;
        let current = load_candidate_by_id_tx(self, &mut tx, input.candidate_id)
            .await?
            .ok_or_else(not_found)?;
        if current.lifecycle_state != IntakeLifecycleState::ConsentGranted {
            append_conflict_revision_tx(
                &mut tx,
                &current,
                IntakeGateReason::CandidateStateConflict,
                input.now_ms,
            )
            .await?;
            tx.commit().await.map_err(map_sqlx)?;
            return Err(candidate_state_conflict());
        }
        let next_seq = current.revision + 1;
        append_candidate_revision_tx(
            &mut tx,
            input.candidate_id,
            next_seq,
            IntakeLifecycleState::ApprovalGranted,
            None,
            Some(input.approval_binding),
            current.manifest_identity_binding.as_deref(),
            current.registry_owned_payload_digest.as_deref(),
            input.now_ms,
        )
        .await?;
        sqlx::query(
            r#"INSERT INTO mcp_intake_approval_events(
                    candidate_id,seq,approval_state,approval_binding,created_at_ms
               ) VALUES (?,?,'approval_granted',?,?)"#,
        )
        .bind(input.candidate_id)
        .bind(next_seq)
        .bind(input.approval_binding)
        .bind(input.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_intake_candidate(input.candidate_id).await
    }

    pub(crate) async fn record_intake_binding_ready(
        &self,
        input: RecordIntakeBindingReady<'_>,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        if input.registry_owned_payload_digest.is_some() {
            return Err(invalid_request());
        }
        let mut tx = self.begin_immediate().await?;
        let current = load_candidate_by_id_tx(self, &mut tx, input.candidate_id)
            .await?
            .ok_or_else(not_found)?;
        if current.source_facet == IntakeSourceFacet::ApprovedStdioCandidate
            && current.transport == IntakeTransport::Stdio
        {
            tx.commit().await.map_err(map_sqlx)?;
            return Err(unavailable_stdio_payload_binding());
        }
        if current.lifecycle_state != IntakeLifecycleState::ApprovalGranted {
            append_conflict_revision_tx(
                &mut tx,
                &current,
                IntakeGateReason::CandidateStateConflict,
                input.now_ms,
            )
            .await?;
            tx.commit().await.map_err(map_sqlx)?;
            return Err(candidate_state_conflict());
        }
        let approval_binding = current
            .approval_binding
            .as_deref()
            .ok_or_else(candidate_state_conflict)?;
        if let Some(existing_owner) = sqlx::query_scalar::<_, String>(
            "SELECT candidate_id FROM mcp_intake_manifest_identity_claims WHERE manifest_identity_binding = ?",
        )
        .bind(input.manifest_identity_binding)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        {
            if existing_owner != input.candidate_id {
                append_conflict_revision_tx(
                    &mut tx,
                    &current,
                    IntakeGateReason::ManifestIdentityConflict,
                    input.now_ms,
                )
                .await?;
                tx.commit().await.map_err(map_sqlx)?;
                return Err(manifest_identity_conflict());
            }
        } else {
            sqlx::query(
                r#"INSERT INTO mcp_intake_manifest_identity_claims(
                        manifest_identity_binding,candidate_id,created_at_ms
                   ) VALUES (?,?,?)"#,
            )
            .bind(input.manifest_identity_binding)
            .bind(input.candidate_id)
            .bind(input.now_ms)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        }
        let next_seq = current.revision + 1;
        append_candidate_revision_tx(
            &mut tx,
            input.candidate_id,
            next_seq,
            IntakeLifecycleState::BindingReady,
            None,
            Some(approval_binding),
            Some(input.manifest_identity_binding),
            input.registry_owned_payload_digest,
            input.now_ms,
        )
        .await?;
        sqlx::query(
            r#"INSERT INTO mcp_intake_binding_events(
                    candidate_id,seq,binding_state,approval_binding,manifest_identity_binding,
                    registry_owned_payload_digest,created_at_ms
               ) VALUES (?,?,'binding_ready',?,?,?,?)"#,
        )
        .bind(input.candidate_id)
        .bind(next_seq)
        .bind(approval_binding)
        .bind(input.manifest_identity_binding)
        .bind(input.registry_owned_payload_digest)
        .bind(input.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_intake_candidate(input.candidate_id).await
    }

    pub(crate) async fn save_intake_inspection_consent(
        &self,
        input: SaveIntakeInspectionConsent,
    ) -> McpPlatformResult<InspectionConsentRecord> {
        validate_stored_inspection_binding(&input.binding)?;
        if input.binding.consent_lifecycle != InspectionLifecycle::ConsentGranted
            || input.expires_at_ms <= input.created_at_ms
        {
            return Err(inspection_integrity_error());
        }
        let mut tx = self.begin_immediate().await?;
        let current = load_candidate_by_id_tx(self, &mut tx, &input.candidate_id)
            .await?
            .ok_or_else(not_found)?;
        validate_candidate_for_batch1_inspection(&current)?;
        if current.revision != input.binding.candidate_revision
            || current.lifecycle_state != input.candidate_lifecycle_state
            || current.source_facet != input.binding.source_facet
            || current.transport != input.binding.transport
        {
            return Err(candidate_state_conflict());
        }
        sqlx::query(
            r#"INSERT INTO mcp_intake_inspection_consents(
                    consent_id,candidate_id,candidate_revision,candidate_lifecycle_state,source_facet,
                    transport,purpose,consent_binding,consent_lifecycle,policy_revision,
                    expires_at_ms,created_at_ms
               ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)"#,
        )
        .bind(&input.consent_id)
        .bind(&input.candidate_id)
        .bind(input.binding.candidate_revision)
        .bind(input.candidate_lifecycle_state.as_str())
        .bind(input.binding.source_facet.as_str())
        .bind(input.binding.transport.as_str())
        .bind(input.binding.purpose.as_str())
        .bind(&input.binding.consent_binding)
        .bind(input.binding.consent_lifecycle.as_str())
        .bind(input.binding.policy_revision)
        .bind(input.expires_at_ms)
        .bind(input.created_at_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_intake_inspection_consent(&input.consent_id).await
    }

    pub(crate) async fn get_intake_inspection_consent(
        &self,
        consent_id: &str,
    ) -> McpPlatformResult<InspectionConsentRecord> {
        self.verify_integrity().await?;
        let row = sqlx::query(INTAKE_INSPECTION_CONSENT_SQL)
            .bind(consent_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        decode_inspection_consent_row(&row)
    }

    pub(crate) async fn record_intake_inspection_snapshot(
        &self,
        input: RecordIntakeInspectionSnapshot,
    ) -> McpPlatformResult<StoredInspectionSnapshot> {
        validate_snapshot_state(&input.snapshot)?;
        let mut tx = self.begin_immediate().await?;
        let current = load_candidate_by_id_tx(self, &mut tx, &input.snapshot.candidate_id)
            .await?
            .ok_or_else(not_found)?;
        validate_candidate_for_batch1_inspection(&current)?;
        let consent = load_intake_inspection_consent_tx(&mut tx, &input.snapshot.consent_id)
            .await?
            .ok_or_else(not_found)?;
        validate_snapshot_against_current(&input, &current, &consent)?;
        sqlx::query(
            r#"INSERT INTO mcp_intake_inspection_snapshots(
                    snapshot_id,candidate_id,candidate_revision,source_facet,transport,purpose,
                    consent_id,consent_binding,consent_lifecycle,policy_revision,inspection_state,
                    observation_surface,operation_phase,failure_family,summary_code,
                    conflict_codes_json,fact_codes_json,reusable,created_at_ms
               ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
        )
        .bind(&input.snapshot.snapshot_id)
        .bind(&input.snapshot.candidate_id)
        .bind(input.snapshot.binding.candidate_revision)
        .bind(input.snapshot.binding.source_facet.as_str())
        .bind(input.snapshot.binding.transport.as_str())
        .bind(input.snapshot.binding.purpose.as_str())
        .bind(&input.snapshot.consent_id)
        .bind(&input.snapshot.binding.consent_binding)
        .bind(input.snapshot.binding.consent_lifecycle.as_str())
        .bind(input.snapshot.binding.policy_revision)
        .bind(input.snapshot.inspection_state.as_str())
        .bind(input.snapshot.observation_surface.as_str())
        .bind(input.snapshot.operation_phase.as_str())
        .bind(
            input
                .snapshot
                .failure_family
                .map(InspectionFailureFamily::as_str),
        )
        .bind(input.snapshot.summary_code.as_str())
        .bind(encode(&input.snapshot.conflict_codes)?)
        .bind(encode(&input.snapshot.fact_codes)?)
        .bind(input.snapshot.reusable)
        .bind(input.snapshot.created_at_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            r#"INSERT INTO mcp_intake_inspection_consent_consumptions(
                    consent_id,snapshot_id,candidate_id,candidate_revision,created_at_ms
               ) VALUES (?,?,?,?,?)"#,
        )
        .bind(&input.snapshot.consent_id)
        .bind(&input.snapshot.snapshot_id)
        .bind(&input.snapshot.candidate_id)
        .bind(input.snapshot.binding.candidate_revision)
        .bind(input.snapshot.created_at_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_intake_inspection_snapshot(&input.snapshot.snapshot_id)
            .await
    }

    pub(crate) async fn get_intake_inspection_snapshot(
        &self,
        snapshot_id: &str,
    ) -> McpPlatformResult<StoredInspectionSnapshot> {
        self.verify_integrity().await?;
        let row = sqlx::query(INTAKE_INSPECTION_SNAPSHOT_SQL)
            .bind(snapshot_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        decode_inspection_snapshot_row(&row)
    }

    async fn save_remote_inspection_consent_v25_tx(
        &self,
        tx: &mut super::AnchoredTransaction<'_>,
        reservation: &RemoteInspectionReservationRecord,
        consent_id: &str,
        candidate_lifecycle_state: IntakeLifecycleState,
        expires_at_ms: i64,
        verified_tuple: &AuthorityVerifiedOpaqueTuple,
        now_ms: i64,
    ) -> McpPlatformResult<RemoteInspectionConsentRecord> {
        reject_catalog_remote_transport(reservation.source_facet, reservation.transport)?;
        let record = RemoteInspectionConsentRecord {
            consent_id: consent_id.to_string(),
            candidate_id: reservation.candidate_id.clone(),
            candidate_revision: reservation.candidate_revision,
            candidate_lifecycle_state,
            source_facet: reservation.source_facet,
            transport: reservation.transport,
            purpose: reservation.purpose,
            policy_revision: reservation.policy_revision,
            expires_at_ms,
            authority_provider: verified_tuple.authority_provider(),
            provider_key_epoch: verified_tuple.provider_key_epoch(),
            generation: verified_tuple.generation(),
            has_authority_record_ref: true,
            created_at_ms: now_ms,
        };
        validate_remote_inspection_consent_record(&record)?;
        sqlx::query(
            r#"INSERT INTO mcp_intake_remote_inspection_consents(
                    consent_id,candidate_id,candidate_revision,candidate_lifecycle_state,
                    source_facet,transport,purpose,policy_revision,expires_at_ms,target_handle,
                    target_identity_binding,authority_provider_id,provider_key_epoch,generation,
                    created_at_ms
               ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
        )
        .bind(&record.consent_id)
        .bind(&record.candidate_id)
        .bind(record.candidate_revision)
        .bind(record.candidate_lifecycle_state.as_str())
        .bind(record.source_facet.as_str())
        .bind(record.transport.as_str())
        .bind(v25_persistence_purpose_for_remote_inspection(
            record.purpose,
        ))
        .bind(record.policy_revision)
        .bind(record.expires_at_ms)
        .bind(verified_tuple.target_handle().as_str())
        .bind(verified_tuple.target_identity_binding().as_str())
        .bind(record.authority_provider.as_str())
        .bind(record.provider_key_epoch)
        .bind(record.generation)
        .bind(record.created_at_ms)
        .execute(&mut ***tx)
        .await
        .map_err(map_sqlx)?;
        Ok(record)
    }

    pub(crate) async fn get_remote_inspection_consent(
        &self,
        consent_id: &str,
    ) -> McpPlatformResult<RemoteInspectionConsentRecord> {
        self.verify_integrity().await?;
        let row = sqlx::query(REMOTE_INSPECTION_CONSENT_SQL)
            .bind(consent_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        decode_remote_inspection_consent_row(&row)
    }

    async fn claim_remote_inspection_attempt_v25_tx(
        &self,
        tx: &mut super::AnchoredTransaction<'_>,
        consent_id: &str,
        attempt_id: &str,
        claim_nonce: &str,
        claimed_at_ms: i64,
        claim_expires_at_ms: i64,
    ) -> McpPlatformResult<RemoteInspectionAttemptRecord> {
        let attempt = build_remote_inspection_attempt(
            attempt_id,
            consent_id,
            claim_nonce,
            claimed_at_ms,
            claim_expires_at_ms,
        )
        .map_err(|failure| failure.as_mcp_error())?;
        let consent = load_remote_inspection_consent_tx(tx, &attempt.consent_id)
            .await?
            .ok_or_else(not_found)?;
        if consent.expires_at_ms <= attempt.claimed_at_ms {
            return Err(remote_attempt_failure(
                RemoteInspectionSafeSubcode::ConsentExpired,
            ));
        }
        if remote_inspection_consumption_exists_tx(tx, &attempt.consent_id).await? {
            return Err(remote_attempt_failure(
                RemoteInspectionSafeSubcode::ConsentConsumed,
            ));
        }
        if load_remote_inspection_active_attempt_tx(tx, &attempt.consent_id)
            .await?
            .is_some()
        {
            return Err(remote_attempt_failure(
                RemoteInspectionSafeSubcode::ActiveAttemptExists,
            ));
        }
        sqlx::query(
            r#"INSERT INTO mcp_intake_remote_inspection_attempts(
                    attempt_id,consent_id,attempt_state,claim_nonce,claimed_at_ms,claim_expires_at_ms,
                    owner_started_at_ms,owner_finished_at_ms,finalized_at_ms
               ) VALUES (?,?,?,?,?,?,?,?,?)"#,
        )
        .bind(&attempt.attempt_id)
        .bind(&attempt.consent_id)
        .bind(attempt.state.as_str())
        .bind(&attempt.claim_nonce)
        .bind(attempt.claimed_at_ms)
        .bind(attempt.claim_expires_at_ms)
        .bind(attempt.owner_started_at_ms)
        .bind(attempt.owner_finished_at_ms)
        .bind(attempt.finalized_at_ms)
        .execute(&mut ***tx)
        .await
        .map_err(map_sqlx)?;
        load_remote_inspection_attempt_tx(tx, &attempt.attempt_id)
            .await?
            .ok_or_else(integrity_error)
    }

    pub(crate) async fn get_remote_inspection_attempt(
        &self,
        attempt_id: &str,
    ) -> McpPlatformResult<RemoteInspectionAttemptRecord> {
        self.verify_integrity().await?;
        let row = sqlx::query(REMOTE_INSPECTION_ATTEMPT_SQL)
            .bind(attempt_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        decode_remote_inspection_attempt_row(&row)
    }

    async fn record_remote_inspection_attempt_owner_started_v25_tx(
        &self,
        tx: &mut super::AnchoredTransaction<'_>,
        attempt_id: &str,
        claim_nonce: &str,
        now_ms: i64,
    ) -> McpPlatformResult<RemoteInspectionAttemptRecord> {
        self.update_remote_inspection_attempt_state_v25_tx(
            tx,
            attempt_id,
            claim_nonce,
            RemoteInspectionAttemptState::OwnerStarted,
            now_ms,
        )
        .await
    }

    async fn record_remote_inspection_attempt_owner_finished_v25_tx(
        &self,
        tx: &mut super::AnchoredTransaction<'_>,
        attempt_id: &str,
        claim_nonce: &str,
        now_ms: i64,
    ) -> McpPlatformResult<RemoteInspectionAttemptRecord> {
        self.update_remote_inspection_attempt_state_v25_tx(
            tx,
            attempt_id,
            claim_nonce,
            RemoteInspectionAttemptState::OwnerFinished,
            now_ms,
        )
        .await
    }

    async fn finalize_remote_inspection_attempt_v25_tx(
        &self,
        tx: &mut super::AnchoredTransaction<'_>,
        consent_id: &str,
        attempt_id: &str,
        snapshot_id: &str,
        claim_nonce: &str,
        safe_subcode: RemoteInspectionSafeSubcode,
        safe_summary: RemoteInspectionSafeSummary,
        finalized_at_ms: i64,
    ) -> McpPlatformResult<RemoteInspectionConsumptionRecord> {
        let current = load_remote_inspection_attempt_tx(tx, attempt_id)
            .await?
            .ok_or_else(not_found)?;
        let finalized = advance_remote_inspection_attempt_state(
            &current,
            claim_nonce,
            RemoteInspectionAttemptState::Finalized,
            finalized_at_ms,
        )
        .map_err(|failure| failure.as_mcp_error())?;
        let consent = load_remote_inspection_consent_tx(tx, consent_id)
            .await?
            .ok_or_else(not_found)?;
        if remote_inspection_consumption_exists_tx(tx, &consent.consent_id).await? {
            return Err(remote_attempt_failure(
                RemoteInspectionSafeSubcode::ConsentConsumed,
            ));
        }
        if consent.expires_at_ms <= finalized_at_ms && current.owner_started_at_ms.is_none() {
            return Err(remote_attempt_failure(
                RemoteInspectionSafeSubcode::ConsentExpired,
            ));
        }
        let binding = load_remote_inspection_binding_row_by_consent_tx(tx, &consent.consent_id)
            .await?
            .ok_or_else(inspection_integrity_error)?;
        let snapshot = RemoteInspectionSnapshotRecord {
            snapshot_id: snapshot_id.to_string(),
            consent_id: consent.consent_id.clone(),
            attempt_id: current.attempt_id.clone(),
            candidate_id: consent.candidate_id.clone(),
            candidate_revision: consent.candidate_revision,
            source_facet: consent.source_facet,
            transport: consent.transport,
            purpose: consent.purpose,
            policy_revision: consent.policy_revision,
            authority_provider: consent.authority_provider,
            provider_key_epoch: consent.provider_key_epoch,
            generation: consent.generation,
            has_authority_record_ref: binding.has_authority_record_ref(),
            safe_subcode,
            safe_summary,
            fail_closed: true,
            created_at_ms: finalized_at_ms,
        };
        validate_remote_inspection_snapshot(&snapshot).map_err(|failure| failure.as_mcp_error())?;
        validate_remote_snapshot_against_consent(&snapshot, &consent, &current)?;
        sqlx::query(
            r#"INSERT INTO mcp_intake_remote_inspection_snapshots(
                    snapshot_id,consent_id,attempt_id,candidate_id,candidate_revision,
                    source_facet,transport,purpose,policy_revision,target_handle,
                    target_identity_binding,authority_provider_id,provider_key_epoch,generation,
                    safe_subcode,safe_summary,fail_closed,created_at_ms
               ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
        )
        .bind(&snapshot.snapshot_id)
        .bind(&snapshot.consent_id)
        .bind(&snapshot.attempt_id)
        .bind(&snapshot.candidate_id)
        .bind(snapshot.candidate_revision)
        .bind(snapshot.source_facet.as_str())
        .bind(snapshot.transport.as_str())
        .bind(v25_persistence_purpose_for_remote_inspection(
            snapshot.purpose,
        ))
        .bind(snapshot.policy_revision)
        .bind(&binding.target_handle)
        .bind(&binding.target_identity_binding)
        .bind(snapshot.authority_provider.as_str())
        .bind(snapshot.provider_key_epoch)
        .bind(snapshot.generation)
        .bind(snapshot.safe_subcode.as_str())
        .bind(snapshot.safe_summary.as_str())
        .bind(snapshot.fail_closed)
        .bind(snapshot.created_at_ms)
        .execute(&mut ***tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            r#"INSERT INTO mcp_intake_remote_inspection_consumptions(
                   consent_id,attempt_id,snapshot_id,candidate_id,candidate_revision,created_at_ms
               ) VALUES (?,?,?,?,?,?)"#,
        )
        .bind(&snapshot.consent_id)
        .bind(&snapshot.attempt_id)
        .bind(&snapshot.snapshot_id)
        .bind(&snapshot.candidate_id)
        .bind(snapshot.candidate_revision)
        .bind(finalized_at_ms)
        .execute(&mut ***tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE mcp_intake_remote_inspection_attempts \
             SET attempt_state = ?, owner_started_at_ms = ?, owner_finished_at_ms = ?, finalized_at_ms = ? \
             WHERE attempt_id = ?",
        )
        .bind(finalized.state.as_str())
        .bind(finalized.owner_started_at_ms)
        .bind(finalized.owner_finished_at_ms)
        .bind(finalized.finalized_at_ms)
        .bind(&finalized.attempt_id)
        .execute(&mut ***tx)
        .await
        .map_err(map_sqlx)?;
        Ok(RemoteInspectionConsumptionRecord {
            consent_id: snapshot.consent_id,
            attempt_id: snapshot.attempt_id,
            snapshot_id: snapshot.snapshot_id,
            candidate_id: snapshot.candidate_id,
            candidate_revision: snapshot.candidate_revision,
            created_at_ms: finalized_at_ms,
        })
    }

    pub(crate) async fn get_remote_inspection_snapshot(
        &self,
        snapshot_id: &str,
    ) -> McpPlatformResult<RemoteInspectionSnapshotRecord> {
        self.verify_integrity().await?;
        let row = sqlx::query(REMOTE_INSPECTION_SNAPSHOT_SQL)
            .bind(snapshot_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        decode_remote_inspection_snapshot_row(&row)
    }

    pub(crate) async fn get_remote_inspection_consumption(
        &self,
        consent_id: &str,
    ) -> McpPlatformResult<RemoteInspectionConsumptionRecord> {
        self.verify_integrity().await?;
        let row = sqlx::query(REMOTE_INSPECTION_CONSUMPTION_SQL)
            .bind(consent_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        decode_remote_inspection_consumption_row(&row)
    }

    async fn update_remote_inspection_attempt_state_v25_tx(
        &self,
        tx: &mut super::AnchoredTransaction<'_>,
        attempt_id: &str,
        claim_nonce: &str,
        next_state: RemoteInspectionAttemptState,
        now_ms: i64,
    ) -> McpPlatformResult<RemoteInspectionAttemptRecord> {
        let current = load_remote_inspection_attempt_tx(tx, attempt_id)
            .await?
            .ok_or_else(not_found)?;
        let next =
            advance_remote_inspection_attempt_state(&current, claim_nonce, next_state, now_ms)
                .map_err(|failure| failure.as_mcp_error())?;
        sqlx::query(
            "UPDATE mcp_intake_remote_inspection_attempts \
             SET attempt_state = ?, owner_started_at_ms = ?, owner_finished_at_ms = ?, finalized_at_ms = ? \
             WHERE attempt_id = ?",
        )
        .bind(next.state.as_str())
        .bind(next.owner_started_at_ms)
        .bind(next.owner_finished_at_ms)
        .bind(next.finalized_at_ms)
        .bind(&next.attempt_id)
        .execute(&mut ***tx)
        .await
        .map_err(map_sqlx)?;
        load_remote_inspection_attempt_tx(tx, attempt_id)
            .await?
            .ok_or_else(integrity_error)
    }

    async fn ensure_remote_reservation_transport_supported(
        &self,
        reservation_id: &str,
        generation: i64,
    ) -> McpPlatformResult<()> {
        let row = sqlx::query(
            r#"SELECT source_facet, transport
               FROM mcp_intake_remote_inspection_reservations
               WHERE reservation_id = ? AND generation = ?
               LIMIT 1"#,
        )
        .bind(reservation_id)
        .bind(generation)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        if let Some(row) = row {
            let source_facet = IntakeSourceFacet::parse(
                &row.try_get::<String, _>("source_facet").map_err(map_sqlx)?,
            )
            .ok_or_else(integrity_error)?;
            let transport =
                IntakeTransport::parse(&row.try_get::<String, _>("transport").map_err(map_sqlx)?)
                    .ok_or_else(integrity_error)?;
            reject_catalog_remote_transport(source_facet, transport)?;
        }
        Ok(())
    }

    async fn ensure_remote_attempt_transport_supported(
        &self,
        attempt_id: &str,
    ) -> McpPlatformResult<()> {
        let row = sqlx::query(
            r#"SELECT c.source_facet, c.transport
               FROM mcp_intake_remote_inspection_attempts a
               JOIN mcp_intake_remote_inspection_consents c ON c.consent_id = a.consent_id
               WHERE a.attempt_id = ?
               LIMIT 1"#,
        )
        .bind(attempt_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        if let Some(row) = row {
            let source_facet = IntakeSourceFacet::parse(
                &row.try_get::<String, _>("source_facet").map_err(map_sqlx)?,
            )
            .ok_or_else(integrity_error)?;
            let transport =
                IntakeTransport::parse(&row.try_get::<String, _>("transport").map_err(map_sqlx)?)
                    .ok_or_else(integrity_error)?;
            reject_catalog_remote_transport(source_facet, transport)?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl RemoteInspectionRepositoryPort for SqliteMcpPlatformRepository {
    async fn reserve_or_load_active_remote_inspection(
        &self,
        input: ReserveOrLoadRemoteInspection<'_>,
    ) -> McpPlatformResult<RemoteInspectionReservationRecord> {
        reject_catalog_remote_transport(input.source_facet, input.transport)?;
        let expected_purpose = RemoteInspectionPurpose::derive(input.source_facet, input.transport)
            .map_err(RemoteInspectionGateError::as_mcp_error)?;
        if expected_purpose != input.purpose
            || input.policy_revision != crate::mcp_platform::intake_remote_inspection::trusted_remote_inspection_policy_revision(expected_purpose)?
        {
            return Err(remote_integrity_failure(
                RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
            ));
        }
        let mut tx = self.begin_immediate().await?;
        let anchored_reservation_id = sqlx::query_scalar::<_, String>(
            r#"SELECT reservation_id
               FROM mcp_intake_remote_inspection_scope_lineage_anchors_v26
               WHERE candidate_id = ? AND candidate_revision = ? AND source_facet = ?
                 AND transport = ? AND purpose = ? AND policy_revision = ?
               LIMIT 1"#,
        )
        .bind(input.candidate_id)
        .bind(input.candidate_revision)
        .bind(input.source_facet.as_str())
        .bind(input.transport.as_str())
        .bind(input.purpose.as_str())
        .bind(input.policy_revision)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .unwrap_or_else(|| input.reservation_id.to_string());

        if anchored_reservation_id == input.reservation_id {
            sqlx::query(
                r#"INSERT OR IGNORE INTO mcp_intake_remote_inspection_scope_lineage_anchors_v26(
                        reservation_id,candidate_id,candidate_revision,source_facet,transport,
                        purpose,policy_revision,created_at_ms
                   ) VALUES (?,?,?,?,?,?,?,?)"#,
            )
            .bind(&anchored_reservation_id)
            .bind(input.candidate_id)
            .bind(input.candidate_revision)
            .bind(input.source_facet.as_str())
            .bind(input.transport.as_str())
            .bind(input.purpose.as_str())
            .bind(input.policy_revision)
            .bind(input.now_ms)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        }

        let latest =
            load_remote_inspection_latest_generation_tx(&mut tx, &anchored_reservation_id).await?;
        let next_generation = match latest.as_ref() {
            None => 1,
            Some(record)
                if record.state == RemoteInspectionReservationState::PrebindReserved
                    && record.intent_fingerprint == input.intent_fingerprint =>
            {
                tx.commit().await.map_err(map_sqlx)?;
                return Ok(record.clone());
            }
            Some(record) if record.state == RemoteInspectionReservationState::PrebindReserved => {
                tx.commit().await.map_err(map_sqlx)?;
                return Err(candidate_state_conflict());
            }
            Some(record)
                if record.state == RemoteInspectionReservationState::PrebindTombstonedTerminal =>
            {
                record.generation + 1
            }
            Some(_) => {
                tx.commit().await.map_err(map_sqlx)?;
                return Err(candidate_state_conflict());
            }
        };

        sqlx::query(
            r#"INSERT INTO mcp_intake_remote_inspection_reservations(
                    reservation_id,generation,candidate_id,candidate_revision,candidate_lifecycle_state,
                    source_facet,transport,purpose,policy_revision,intent_fingerprint,
                    intent_fingerprint_key_id,state,consent_id,claim_attempt_id,
                    no_live_proof_kind,no_live_proof_id,no_live_proof_hmac,no_live_proof_key_epoch,
                    no_live_proof_observed_at_ms,last_event_ordinal,created_at_ms,updated_at_ms
               ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
        )
        .bind(&anchored_reservation_id)
        .bind(next_generation)
        .bind(input.candidate_id)
        .bind(input.candidate_revision)
        .bind(input.candidate_lifecycle_state.as_str())
        .bind(input.source_facet.as_str())
        .bind(input.transport.as_str())
        .bind(input.purpose.as_str())
        .bind(input.policy_revision)
        .bind(input.intent_fingerprint)
        .bind(input.intent_fingerprint_key_id)
        .bind(RemoteInspectionReservationState::PrebindReserved.as_str())
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind(Option::<i64>::None)
        .bind(Option::<i64>::None)
        .bind(1_i64)
        .bind(input.now_ms)
        .bind(input.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let record = load_remote_inspection_reservation_tx(
            &mut tx,
            &anchored_reservation_id,
            next_generation,
        )
        .await?
        .ok_or_else(integrity_error)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(record)
    }

    async fn tombstone_remote_inspection_prebind(
        &self,
        input: TombstoneRemoteInspectionPrebind<'_>,
    ) -> McpPlatformResult<RemoteInspectionReservationRecord> {
        self.ensure_remote_reservation_transport_supported(input.reservation_id, input.generation)
            .await?;
        let mut tx = self.begin_immediate().await?;
        let current =
            load_remote_inspection_reservation_tx(&mut tx, input.reservation_id, input.generation)
                .await?
                .ok_or_else(not_found)?;
        reject_catalog_remote_transport(current.source_facet, current.transport)?;
        if current.state != RemoteInspectionReservationState::PrebindReserved {
            tx.commit().await.map_err(map_sqlx)?;
            return Err(candidate_state_conflict());
        }
        if input.proof.kind() != RemoteInspectionNoLiveProofKind::Tombstoned {
            tx.commit().await.map_err(map_sqlx)?;
            return Err(integrity_error());
        }
        sqlx::query(
            r#"UPDATE mcp_intake_remote_inspection_reservations
               SET state = ?, no_live_proof_kind = ?, no_live_proof_id = ?, no_live_proof_hmac = ?,
                   no_live_proof_key_epoch = ?, no_live_proof_observed_at_ms = ?, last_event_ordinal = ?,
                   updated_at_ms = ?
               WHERE reservation_id = ? AND generation = ?"#,
        )
        .bind(RemoteInspectionReservationState::PrebindTombstonedTerminal.as_str())
        .bind(input.proof.kind().as_str())
        .bind(input.proof.proof_id())
        .bind(input.proof.proof_hmac())
        .bind(input.proof.proof_key_epoch())
        .bind(input.proof.observed_at_ms())
        .bind(current.last_event_ordinal + 1)
        .bind(input.now_ms)
        .bind(input.reservation_id)
        .bind(input.generation)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        append_remote_inspection_event_tx(
            &mut tx,
            input.reservation_id,
            input.generation,
            current.last_event_ordinal + 1,
            RemoteInspectionReservationEventType::PrebindTombstoned,
            current.state,
            RemoteInspectionReservationState::PrebindTombstonedTerminal,
            None,
            None,
            Some(input.proof),
            input.now_ms,
        )
        .await?;
        let record =
            load_remote_inspection_reservation_tx(&mut tx, input.reservation_id, input.generation)
                .await?
                .ok_or_else(integrity_error)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(record)
    }

    async fn bind_remote_inspection_pending_commit(
        &self,
        input: BindRemoteInspectionPendingCommit<'_>,
    ) -> McpPlatformResult<RemoteInspectionReservationRecord> {
        self.ensure_remote_reservation_transport_supported(input.reservation_id, input.generation)
            .await?;
        let mut tx = self.begin_immediate().await?;
        let current =
            load_remote_inspection_reservation_tx(&mut tx, input.reservation_id, input.generation)
                .await?
                .ok_or_else(not_found)?;
        reject_catalog_remote_transport(current.source_facet, current.transport)?;
        if current.state != RemoteInspectionReservationState::PrebindReserved {
            tx.commit().await.map_err(map_sqlx)?;
            return Err(candidate_state_conflict());
        }
        self.save_remote_inspection_consent_v25_tx(
            &mut tx,
            &current,
            input.consent_id,
            input.candidate_lifecycle_state,
            input.expires_at_ms,
            input.verified_tuple,
            input.now_ms,
        )
        .await?;
        sqlx::query(
            r#"INSERT INTO mcp_intake_remote_inspection_bindings_b26(
                    consent_id,reservation_id,reservation_generation,candidate_id,candidate_revision,
                    source_facet,transport,purpose,policy_revision,intent_fingerprint,
                    intent_fingerprint_key_id,target_handle,target_identity_binding,
                    authority_provider_id,authority_key_epoch,authority_generation,
                    authority_record_ref,created_at_ms
               ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
        )
        .bind(input.consent_id)
        .bind(input.reservation_id)
        .bind(input.generation)
        .bind(&current.candidate_id)
        .bind(current.candidate_revision)
        .bind(current.source_facet.as_str())
        .bind(current.transport.as_str())
        .bind(current.purpose.as_str())
        .bind(current.policy_revision)
        .bind(&current.intent_fingerprint)
        .bind(&current.intent_fingerprint_key_id)
        .bind(input.verified_tuple.target_handle().as_str())
        .bind(input.verified_tuple.target_identity_binding().as_str())
        .bind(input.verified_tuple.authority_provider().as_str())
        .bind(input.verified_tuple.provider_key_epoch())
        .bind(input.verified_tuple.generation())
        .bind(input.verified_tuple.authority_record_ref().as_str())
        .bind(input.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            r#"INSERT INTO mcp_intake_remote_inspection_commit_events_v26(
                    consent_id,ordinal,reservation_id,reservation_generation,candidate_id,
                    candidate_revision,source_facet,transport,purpose,policy_revision,
                    intent_fingerprint,intent_fingerprint_key_id,target_handle,
                    target_identity_binding,authority_provider_id,authority_key_epoch,
                    authority_generation,authority_record_ref,commit_state,no_live_proof_kind,
                    no_live_proof_id,no_live_proof_hmac,no_live_proof_key_epoch,
                    no_live_proof_observed_at_ms,created_at_ms
               ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
        )
        .bind(input.consent_id)
        .bind(1_i64)
        .bind(input.reservation_id)
        .bind(input.generation)
        .bind(&current.candidate_id)
        .bind(current.candidate_revision)
        .bind(current.source_facet.as_str())
        .bind(current.transport.as_str())
        .bind(current.purpose.as_str())
        .bind(current.policy_revision)
        .bind(&current.intent_fingerprint)
        .bind(&current.intent_fingerprint_key_id)
        .bind(input.verified_tuple.target_handle().as_str())
        .bind(input.verified_tuple.target_identity_binding().as_str())
        .bind(input.verified_tuple.authority_provider().as_str())
        .bind(input.verified_tuple.provider_key_epoch())
        .bind(input.verified_tuple.generation())
        .bind(input.verified_tuple.authority_record_ref().as_str())
        .bind(RemoteInspectionCommitState::PendingAuthorityCommit.as_str())
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind(Option::<i64>::None)
        .bind(Option::<i64>::None)
        .bind(input.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            r#"UPDATE mcp_intake_remote_inspection_reservations
               SET state = ?, consent_id = ?, last_event_ordinal = ?, updated_at_ms = ?
               WHERE reservation_id = ? AND generation = ?"#,
        )
        .bind(RemoteInspectionReservationState::PostbindPendingCommit.as_str())
        .bind(input.consent_id)
        .bind(current.last_event_ordinal + 1)
        .bind(input.now_ms)
        .bind(input.reservation_id)
        .bind(input.generation)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        append_remote_inspection_event_tx(
            &mut tx,
            input.reservation_id,
            input.generation,
            current.last_event_ordinal + 1,
            RemoteInspectionReservationEventType::PostbindBound,
            current.state,
            RemoteInspectionReservationState::PostbindPendingCommit,
            Some(input.consent_id),
            None,
            None,
            input.now_ms,
        )
        .await?;
        let record =
            load_remote_inspection_reservation_tx(&mut tx, input.reservation_id, input.generation)
                .await?
                .ok_or_else(integrity_error)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(record)
    }

    async fn record_remote_inspection_commit_outcome(
        &self,
        input: RecordRemoteInspectionCommitOutcome<'_>,
    ) -> McpPlatformResult<RemoteInspectionReservationRecord> {
        self.ensure_remote_reservation_transport_supported(input.reservation_id, input.generation)
            .await?;
        let mut tx = self.begin_immediate().await?;
        let current =
            load_remote_inspection_reservation_tx(&mut tx, input.reservation_id, input.generation)
                .await?
                .ok_or_else(not_found)?;
        reject_catalog_remote_transport(current.source_facet, current.transport)?;
        if !matches!(
            current.state,
            RemoteInspectionReservationState::PostbindPendingCommit
                | RemoteInspectionReservationState::ReconcileRequired
        ) {
            tx.commit().await.map_err(map_sqlx)?;
            return Err(candidate_state_conflict());
        }

        let (next_state, event_type) = if input.outcome.is_committed() {
            (
                RemoteInspectionReservationState::CommittedClaimable,
                RemoteInspectionReservationEventType::AuthorityCommitted,
            )
        } else if input.outcome.is_reconcile_required() {
            (
                RemoteInspectionReservationState::ReconcileRequired,
                RemoteInspectionReservationEventType::AuthorityReconcileRequired,
            )
        } else {
            (
                RemoteInspectionReservationState::PostbindBlockedTerminal,
                RemoteInspectionReservationEventType::PostbindBlockedNoLive,
            )
        };
        let proof = input.outcome.blocked_proof();

        if let Some(proof) = proof {
            if proof.kind() != RemoteInspectionNoLiveProofKind::DefinitiveNoLivePostbind {
                tx.commit().await.map_err(map_sqlx)?;
                return Err(integrity_error());
            }
        }
        let binding =
            load_remote_inspection_binding_row_tx(&mut tx, input.reservation_id, input.generation)
                .await?
                .ok_or_else(integrity_error)?;
        if let Some(commit_state) = commit_state_for_outcome(input.outcome) {
            sqlx::query(
                r#"INSERT INTO mcp_intake_remote_inspection_commit_events_v26(
                        consent_id,ordinal,reservation_id,reservation_generation,candidate_id,
                        candidate_revision,source_facet,transport,purpose,policy_revision,
                        intent_fingerprint,intent_fingerprint_key_id,target_handle,
                        target_identity_binding,authority_provider_id,authority_key_epoch,
                        authority_generation,authority_record_ref,commit_state,no_live_proof_kind,
                        no_live_proof_id,no_live_proof_hmac,no_live_proof_key_epoch,
                        no_live_proof_observed_at_ms,created_at_ms
                   ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
            )
            .bind(input.consent_id)
            .bind(2_i64)
            .bind(input.reservation_id)
            .bind(input.generation)
            .bind(&current.candidate_id)
            .bind(current.candidate_revision)
            .bind(current.source_facet.as_str())
            .bind(current.transport.as_str())
            .bind(current.purpose.as_str())
            .bind(current.policy_revision)
            .bind(&current.intent_fingerprint)
            .bind(&current.intent_fingerprint_key_id)
            .bind(&binding.target_handle)
            .bind(&binding.target_identity_binding)
            .bind(binding.authority_provider.as_str())
            .bind(binding.authority_key_epoch)
            .bind(binding.authority_generation)
            .bind(&binding.authority_record_ref)
            .bind(commit_state.as_str())
            .bind(proof.map(|value| value.kind().as_str()))
            .bind(proof.map(VerifiedNoLiveProof::proof_id))
            .bind(proof.map(VerifiedNoLiveProof::proof_hmac))
            .bind(proof.map(VerifiedNoLiveProof::proof_key_epoch))
            .bind(proof.map(VerifiedNoLiveProof::observed_at_ms))
            .bind(input.now_ms)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        }

        sqlx::query(
            r#"UPDATE mcp_intake_remote_inspection_reservations
               SET state = ?, no_live_proof_kind = ?, no_live_proof_id = ?, no_live_proof_hmac = ?,
                   no_live_proof_key_epoch = ?, no_live_proof_observed_at_ms = ?, last_event_ordinal = ?,
                   updated_at_ms = ?
               WHERE reservation_id = ? AND generation = ?"#,
        )
        .bind(next_state.as_str())
        .bind(proof.map(|value| value.kind().as_str()))
        .bind(proof.map(VerifiedNoLiveProof::proof_id))
        .bind(proof.map(VerifiedNoLiveProof::proof_hmac))
        .bind(proof.map(VerifiedNoLiveProof::proof_key_epoch))
        .bind(proof.map(VerifiedNoLiveProof::observed_at_ms))
        .bind(current.last_event_ordinal + 1)
        .bind(input.now_ms)
        .bind(input.reservation_id)
        .bind(input.generation)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        append_remote_inspection_event_tx(
            &mut tx,
            input.reservation_id,
            input.generation,
            current.last_event_ordinal + 1,
            event_type,
            current.state,
            next_state,
            Some(input.consent_id),
            None,
            proof,
            input.now_ms,
        )
        .await?;
        let record =
            load_remote_inspection_reservation_tx(&mut tx, input.reservation_id, input.generation)
                .await?
                .ok_or_else(integrity_error)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(record)
    }

    async fn claim_committed_remote_inspection_attempt(
        &self,
        input: ClaimCommittedRemoteInspectionAttempt<'_>,
    ) -> McpPlatformResult<RemoteInspectionAttemptRecord> {
        self.ensure_remote_reservation_transport_supported(input.reservation_id, input.generation)
            .await?;
        let mut tx = self.begin_immediate().await?;
        let current =
            load_remote_inspection_reservation_tx(&mut tx, input.reservation_id, input.generation)
                .await?
                .ok_or_else(not_found)?;
        reject_catalog_remote_transport(current.source_facet, current.transport)?;
        let stale_attempt = match current.state {
            RemoteInspectionReservationState::CommittedClaimable => None,
            RemoteInspectionReservationState::Claimed
                if current.consent_id.as_deref() == Some(input.consent_id) =>
            {
                let stale_attempt_id = current
                    .claim_attempt_id
                    .as_deref()
                    .ok_or_else(integrity_error)?;
                Some(
                    load_remote_inspection_attempt_tx(&mut tx, stale_attempt_id)
                        .await?
                        .ok_or_else(integrity_error)?,
                )
            }
            _ => {
                tx.commit().await.map_err(map_sqlx)?;
                return Err(candidate_state_conflict());
            }
        };
        if current.consent_id.as_deref() != Some(input.consent_id) {
            tx.commit().await.map_err(map_sqlx)?;
            return Err(candidate_state_conflict());
        }
        let committed = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM mcp_intake_remote_inspection_commit_events_v26 \
             WHERE consent_id = ? AND ordinal = 2 AND commit_state = ? LIMIT 1",
        )
        .bind(input.consent_id)
        .bind(RemoteInspectionCommitState::Committed.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .is_some();
        if !committed {
            tx.commit().await.map_err(map_sqlx)?;
            return Err(candidate_state_conflict());
        }
        let (from_state, claimed_event_ordinal) = if let Some(stale_attempt) = stale_attempt {
            let recovered =
                recover_remote_inspection_attempt_stale_claim(&stale_attempt, input.claimed_at_ms)
                    .map_err(|failure| failure.as_mcp_error())?;
            let finalized = sqlx::query(
                "UPDATE mcp_intake_remote_inspection_attempts \
                 SET attempt_state = ?, owner_started_at_ms = ?, owner_finished_at_ms = ?, finalized_at_ms = ? \
                 WHERE attempt_id = ? AND consent_id = ? AND attempt_state = ? \
                   AND owner_started_at_ms IS NULL AND owner_finished_at_ms IS NULL \
                   AND finalized_at_ms IS NULL AND claim_expires_at_ms <= ?",
            )
            .bind(recovered.state.as_str())
            .bind(recovered.owner_started_at_ms)
            .bind(recovered.owner_finished_at_ms)
            .bind(recovered.finalized_at_ms)
            .bind(&recovered.attempt_id)
            .bind(&recovered.consent_id)
            .bind(RemoteInspectionAttemptState::Claimed.as_str())
            .bind(input.claimed_at_ms)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?
            .rows_affected();
            if finalized != 1 {
                tx.commit().await.map_err(map_sqlx)?;
                return Err(candidate_state_conflict());
            }
            let released = sqlx::query(
                r#"UPDATE mcp_intake_remote_inspection_reservations
                   SET state = ?, claim_attempt_id = NULL, last_event_ordinal = ?, updated_at_ms = ?
                   WHERE reservation_id = ? AND generation = ? AND state = ? AND consent_id = ?
                     AND claim_attempt_id = ? AND last_event_ordinal = ?"#,
            )
            .bind(RemoteInspectionReservationState::CommittedClaimable.as_str())
            .bind(current.last_event_ordinal + 1)
            .bind(input.claimed_at_ms)
            .bind(input.reservation_id)
            .bind(input.generation)
            .bind(RemoteInspectionReservationState::Claimed.as_str())
            .bind(input.consent_id)
            .bind(&stale_attempt.attempt_id)
            .bind(current.last_event_ordinal)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?
            .rows_affected();
            if released != 1 {
                tx.commit().await.map_err(map_sqlx)?;
                return Err(candidate_state_conflict());
            }
            append_remote_inspection_event_tx(
                &mut tx,
                input.reservation_id,
                input.generation,
                current.last_event_ordinal + 1,
                RemoteInspectionReservationEventType::ClaimExpired,
                RemoteInspectionReservationState::Claimed,
                RemoteInspectionReservationState::CommittedClaimable,
                Some(input.consent_id),
                Some(&stale_attempt.attempt_id),
                None,
                input.claimed_at_ms,
            )
            .await?;
            (
                RemoteInspectionReservationState::CommittedClaimable,
                current.last_event_ordinal + 2,
            )
        } else {
            (current.state, current.last_event_ordinal + 1)
        };
        let attempt = self
            .claim_remote_inspection_attempt_v25_tx(
                &mut tx,
                input.consent_id,
                input.attempt_id,
                input.claim_nonce,
                input.claimed_at_ms,
                input.claim_expires_at_ms,
            )
            .await?;
        let claimed = sqlx::query(
            r#"UPDATE mcp_intake_remote_inspection_reservations
               SET state = ?, claim_attempt_id = ?, last_event_ordinal = ?, updated_at_ms = ?
               WHERE reservation_id = ? AND generation = ? AND state = ? AND consent_id = ?
                 AND claim_attempt_id IS NULL AND last_event_ordinal = ?"#,
        )
        .bind(RemoteInspectionReservationState::Claimed.as_str())
        .bind(&attempt.attempt_id)
        .bind(claimed_event_ordinal)
        .bind(input.claimed_at_ms)
        .bind(input.reservation_id)
        .bind(input.generation)
        .bind(RemoteInspectionReservationState::CommittedClaimable.as_str())
        .bind(input.consent_id)
        .bind(claimed_event_ordinal - 1)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .rows_affected();
        if claimed != 1 {
            tx.commit().await.map_err(map_sqlx)?;
            return Err(candidate_state_conflict());
        }
        append_remote_inspection_event_tx(
            &mut tx,
            input.reservation_id,
            input.generation,
            claimed_event_ordinal,
            RemoteInspectionReservationEventType::Claimed,
            from_state,
            RemoteInspectionReservationState::Claimed,
            Some(input.consent_id),
            Some(&attempt.attempt_id),
            None,
            input.claimed_at_ms,
        )
        .await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(attempt)
    }

    async fn record_remote_inspection_owner_started(
        &self,
        attempt_id: &str,
        claim_nonce: &str,
        now_ms: i64,
    ) -> McpPlatformResult<RemoteInspectionAttemptRecord> {
        self.ensure_remote_attempt_transport_supported(attempt_id)
            .await?;
        let mut tx = self.begin_immediate().await?;
        let reservation = load_remote_inspection_reservation_by_attempt_tx(&mut tx, attempt_id)
            .await?
            .ok_or_else(not_found)?;
        let attempt = self
            .record_remote_inspection_attempt_owner_started_v25_tx(
                &mut tx,
                attempt_id,
                claim_nonce,
                now_ms,
            )
            .await?;
        sqlx::query(
            r#"UPDATE mcp_intake_remote_inspection_reservations
               SET state = ?, last_event_ordinal = ?, updated_at_ms = ?
               WHERE reservation_id = ? AND generation = ?"#,
        )
        .bind(RemoteInspectionReservationState::OwnerStarted.as_str())
        .bind(reservation.last_event_ordinal + 1)
        .bind(now_ms)
        .bind(&reservation.reservation_id)
        .bind(reservation.generation)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        append_remote_inspection_event_tx(
            &mut tx,
            &reservation.reservation_id,
            reservation.generation,
            reservation.last_event_ordinal + 1,
            RemoteInspectionReservationEventType::OwnerStarted,
            reservation.state,
            RemoteInspectionReservationState::OwnerStarted,
            reservation.consent_id.as_deref(),
            Some(attempt_id),
            None,
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(attempt)
    }

    async fn record_remote_inspection_owner_finished(
        &self,
        attempt_id: &str,
        claim_nonce: &str,
        now_ms: i64,
    ) -> McpPlatformResult<RemoteInspectionAttemptRecord> {
        self.ensure_remote_attempt_transport_supported(attempt_id)
            .await?;
        let mut tx = self.begin_immediate().await?;
        let reservation = load_remote_inspection_reservation_by_attempt_tx(&mut tx, attempt_id)
            .await?
            .ok_or_else(not_found)?;
        let attempt = self
            .record_remote_inspection_attempt_owner_finished_v25_tx(
                &mut tx,
                attempt_id,
                claim_nonce,
                now_ms,
            )
            .await?;
        sqlx::query(
            r#"UPDATE mcp_intake_remote_inspection_reservations
               SET state = ?, last_event_ordinal = ?, updated_at_ms = ?
               WHERE reservation_id = ? AND generation = ?"#,
        )
        .bind(RemoteInspectionReservationState::OwnerFinished.as_str())
        .bind(reservation.last_event_ordinal + 1)
        .bind(now_ms)
        .bind(&reservation.reservation_id)
        .bind(reservation.generation)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        append_remote_inspection_event_tx(
            &mut tx,
            &reservation.reservation_id,
            reservation.generation,
            reservation.last_event_ordinal + 1,
            RemoteInspectionReservationEventType::OwnerFinished,
            reservation.state,
            RemoteInspectionReservationState::OwnerFinished,
            reservation.consent_id.as_deref(),
            Some(attempt_id),
            None,
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(attempt)
    }

    async fn finalize_remote_inspection_consumption(
        &self,
        input: FinalizeRemoteInspectionConsumption<'_>,
    ) -> McpPlatformResult<RemoteInspectionConsumptionRecord> {
        self.ensure_remote_reservation_transport_supported(input.reservation_id, input.generation)
            .await?;
        let mut tx = self.begin_immediate().await?;
        let reservation =
            load_remote_inspection_reservation_tx(&mut tx, input.reservation_id, input.generation)
                .await?
                .ok_or_else(not_found)?;
        if reservation.state != RemoteInspectionReservationState::OwnerFinished
            || reservation.consent_id.as_deref() != Some(input.consent_id)
            || reservation.claim_attempt_id.as_deref() != Some(input.attempt_id)
        {
            tx.commit().await.map_err(map_sqlx)?;
            return Err(candidate_state_conflict());
        }
        let consumption = self
            .finalize_remote_inspection_attempt_v25_tx(
                &mut tx,
                input.consent_id,
                input.attempt_id,
                input.snapshot_id,
                input.claim_nonce,
                input.safe_subcode,
                input.safe_summary,
                input.finalized_at_ms,
            )
            .await?;
        sqlx::query(
            r#"UPDATE mcp_intake_remote_inspection_reservations
               SET state = ?, last_event_ordinal = ?, updated_at_ms = ?
               WHERE reservation_id = ? AND generation = ?"#,
        )
        .bind(RemoteInspectionReservationState::ConsumedFinalized.as_str())
        .bind(reservation.last_event_ordinal + 1)
        .bind(input.finalized_at_ms)
        .bind(input.reservation_id)
        .bind(input.generation)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        append_remote_inspection_event_tx(
            &mut tx,
            input.reservation_id,
            input.generation,
            reservation.last_event_ordinal + 1,
            RemoteInspectionReservationEventType::ConsumedFinalized,
            reservation.state,
            RemoteInspectionReservationState::ConsumedFinalized,
            Some(input.consent_id),
            Some(input.attempt_id),
            None,
            input.finalized_at_ms,
        )
        .await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(consumption)
    }
}

const LATEST_CANDIDATE_SQL: &str = r#"SELECT c.candidate_id,c.source_facet,c.transport,
       c.submission_binding,c.redacted_configuration_ref,c.descriptor_digest,c.created_at_ms,
       r.seq,r.lifecycle_state,r.gate_reason,r.approval_binding,r.manifest_identity_binding,
       r.registry_owned_payload_digest,r.created_at_ms AS updated_at_ms,
       cfg.source_facet AS configuration_source_facet,cfg.private_reference AS configuration_private_reference
FROM mcp_intake_candidates c
JOIN mcp_intake_configuration_refs cfg
  ON cfg.configuration_ref = c.redacted_configuration_ref
JOIN mcp_intake_candidate_revisions r
  ON r.candidate_id = c.candidate_id
WHERE c.candidate_id = ?
ORDER BY r.seq DESC
LIMIT 1"#;

const LATEST_CANDIDATE_BY_SUBMISSION_SQL: &str = r#"SELECT c.candidate_id,c.source_facet,c.transport,
       c.submission_binding,c.redacted_configuration_ref,c.descriptor_digest,c.created_at_ms,
       r.seq,r.lifecycle_state,r.gate_reason,r.approval_binding,r.manifest_identity_binding,
       r.registry_owned_payload_digest,r.created_at_ms AS updated_at_ms,
       cfg.source_facet AS configuration_source_facet,cfg.private_reference AS configuration_private_reference
FROM mcp_intake_candidates c
JOIN mcp_intake_configuration_refs cfg
  ON cfg.configuration_ref = c.redacted_configuration_ref
JOIN mcp_intake_candidate_revisions r
  ON r.candidate_id = c.candidate_id
WHERE c.submission_binding = ?
ORDER BY r.seq DESC
LIMIT 1"#;

const INTAKE_INSPECTION_CONSENT_SQL: &str = r#"SELECT c.consent_id,c.candidate_id,c.candidate_revision,
       c.candidate_lifecycle_state,c.source_facet,c.transport,c.purpose,c.consent_binding,
       c.consent_lifecycle,c.policy_revision,c.expires_at_ms,c.created_at_ms,
       cc.created_at_ms AS consumed_at_ms
FROM mcp_intake_inspection_consents c
LEFT JOIN mcp_intake_inspection_consent_consumptions cc
  ON cc.consent_id = c.consent_id
WHERE c.consent_id = ?
LIMIT 1"#;

const INTAKE_INSPECTION_SNAPSHOT_SQL: &str = r#"SELECT snapshot_id,candidate_id,candidate_revision,
       source_facet,transport,purpose,consent_id,consent_binding,consent_lifecycle,
       policy_revision,inspection_state,observation_surface,operation_phase,failure_family,
       summary_code,conflict_codes_json,fact_codes_json,reusable,created_at_ms
FROM mcp_intake_inspection_snapshots
WHERE snapshot_id = ?
LIMIT 1"#;

const REMOTE_INSPECTION_CONSENT_SQL: &str = r#"SELECT consent_id,candidate_id,candidate_revision,
       candidate_lifecycle_state,source_facet,transport,purpose,policy_revision,expires_at_ms,
       authority_provider_id,provider_key_epoch,generation,
       EXISTS(
           SELECT 1
           FROM mcp_intake_remote_inspection_bindings_b26 binding
           WHERE binding.consent_id = consent.consent_id
             AND length(binding.authority_record_ref) > 0
       ) AS has_authority_record_ref,
       created_at_ms
FROM mcp_intake_remote_inspection_consents consent
WHERE consent_id = ?
LIMIT 1"#;

const REMOTE_INSPECTION_ATTEMPT_SQL: &str = r#"SELECT attempt_id,consent_id,attempt_state,
       claim_nonce,claimed_at_ms,claim_expires_at_ms,owner_started_at_ms,owner_finished_at_ms,finalized_at_ms
FROM mcp_intake_remote_inspection_attempts
WHERE attempt_id = ?
LIMIT 1"#;

const REMOTE_INSPECTION_SNAPSHOT_SQL: &str = r#"SELECT snapshot_id,consent_id,attempt_id,
       candidate_id,candidate_revision,source_facet,transport,purpose,policy_revision,
       authority_provider_id,provider_key_epoch,generation,
       EXISTS(
           SELECT 1
           FROM mcp_intake_remote_inspection_bindings_b26 binding
           WHERE binding.consent_id = snapshot.consent_id
             AND length(binding.authority_record_ref) > 0
       ) AS has_authority_record_ref,
       safe_subcode,safe_summary,fail_closed,created_at_ms
FROM mcp_intake_remote_inspection_snapshots snapshot
WHERE snapshot_id = ?
LIMIT 1"#;

const REMOTE_INSPECTION_CONSUMPTION_SQL: &str = r#"SELECT consent_id,attempt_id,snapshot_id,
       candidate_id,candidate_revision,created_at_ms
FROM mcp_intake_remote_inspection_consumptions
WHERE consent_id = ?
LIMIT 1"#;

const REMOTE_INSPECTION_RESERVATION_SQL: &str = r#"SELECT reservation_id,generation,candidate_id,
       candidate_revision,candidate_lifecycle_state,source_facet,transport,purpose,policy_revision,
       intent_fingerprint,intent_fingerprint_key_id,state,consent_id,claim_attempt_id,
       no_live_proof_kind,no_live_proof_id,no_live_proof_hmac,no_live_proof_key_epoch,
       no_live_proof_observed_at_ms,
       (
           SELECT binding.authority_provider_id
           FROM mcp_intake_remote_inspection_bindings_b26 binding
           WHERE binding.reservation_id = reservation.reservation_id
             AND binding.reservation_generation = reservation.generation
           LIMIT 1
       ) AS authority_provider_id,
       (
           SELECT binding.authority_key_epoch
           FROM mcp_intake_remote_inspection_bindings_b26 binding
           WHERE binding.reservation_id = reservation.reservation_id
             AND binding.reservation_generation = reservation.generation
           LIMIT 1
       ) AS authority_key_epoch,
       (
           SELECT binding.authority_generation
           FROM mcp_intake_remote_inspection_bindings_b26 binding
           WHERE binding.reservation_id = reservation.reservation_id
             AND binding.reservation_generation = reservation.generation
           LIMIT 1
       ) AS authority_generation,
       EXISTS(
           SELECT 1
           FROM mcp_intake_remote_inspection_bindings_b26 binding
           WHERE binding.reservation_id = reservation.reservation_id
             AND binding.reservation_generation = reservation.generation
             AND length(binding.authority_record_ref) > 0
       ) AS has_authority_record_ref,
       last_event_ordinal,created_at_ms,updated_at_ms
FROM mcp_intake_remote_inspection_reservations reservation
WHERE reservation_id = ? AND generation = ?
LIMIT 1"#;

const REMOTE_INSPECTION_BINDING_SQL: &str = r#"SELECT target_handle,target_identity_binding,
       authority_provider_id,authority_key_epoch,authority_generation,authority_record_ref
FROM mcp_intake_remote_inspection_bindings_b26
WHERE reservation_id = ? AND reservation_generation = ?
LIMIT 1"#;

const REMOTE_INSPECTION_BINDING_BY_CONSENT_SQL: &str = r#"SELECT target_handle,
       target_identity_binding,authority_provider_id,authority_key_epoch,authority_generation,
       authority_record_ref
FROM mcp_intake_remote_inspection_bindings_b26
WHERE consent_id = ?
LIMIT 1"#;

fn configuration_source_facet(descriptor: &IntakeConfigurationDescriptor) -> IntakeSourceFacet {
    match descriptor {
        IntakeConfigurationDescriptor::CatalogPlanning { .. } => IntakeSourceFacet::CatalogPlanning,
        IntakeConfigurationDescriptor::ManualHttpsCandidate { .. } => {
            IntakeSourceFacet::ManualHttpsCandidate
        }
        IntakeConfigurationDescriptor::ApprovedStdioCandidate { .. } => {
            IntakeSourceFacet::ApprovedStdioCandidate
        }
        IntakeConfigurationDescriptor::LegacyQuarantine { .. } => {
            IntakeSourceFacet::LegacyQuarantine
        }
    }
}

fn validate_initial_candidate_state(
    source_facet: IntakeSourceFacet,
    transport: IntakeTransport,
    initial_state: IntakeLifecycleState,
) -> McpPlatformResult<()> {
    if source_facet == IntakeSourceFacet::LegacyQuarantine {
        if transport != IntakeTransport::Legacy
            || initial_state != IntakeLifecycleState::LegacyQuarantine
        {
            return Err(candidate_state_conflict());
        }
        return Ok(());
    }
    if !matches!(
        initial_state,
        IntakeLifecycleState::Submitted | IntakeLifecycleState::AwaitingConsent
    ) {
        return Err(candidate_state_conflict());
    }
    if transport == IntakeTransport::Legacy {
        return Err(candidate_state_conflict());
    }
    Ok(())
}

async fn append_conflict_revision_tx(
    tx: &mut super::AnchoredTransaction<'_>,
    current: &IntakeCandidateRecord,
    gate_reason: IntakeGateReason,
    now_ms: i64,
) -> McpPlatformResult<()> {
    append_candidate_revision_tx(
        tx,
        &current.candidate_id,
        current.revision + 1,
        current.lifecycle_state,
        Some(gate_reason),
        current.approval_binding.as_deref(),
        current.manifest_identity_binding.as_deref(),
        current.registry_owned_payload_digest.as_deref(),
        now_ms,
    )
    .await
}

async fn append_candidate_revision_tx(
    tx: &mut super::AnchoredTransaction<'_>,
    candidate_id: &str,
    seq: i64,
    lifecycle_state: IntakeLifecycleState,
    gate_reason: Option<IntakeGateReason>,
    approval_binding: Option<&str>,
    manifest_identity_binding: Option<&str>,
    registry_owned_payload_digest: Option<&str>,
    now_ms: i64,
) -> McpPlatformResult<()> {
    sqlx::query(
        r#"INSERT INTO mcp_intake_candidate_revisions(
                candidate_id,seq,lifecycle_state,gate_reason,approval_binding,manifest_identity_binding,
                registry_owned_payload_digest,created_at_ms
           ) VALUES (?,?,?,?,?,?,?,?)"#,
    )
    .bind(candidate_id)
    .bind(seq)
    .bind(lifecycle_state.as_str())
    .bind(gate_reason.map(IntakeGateReason::as_str))
    .bind(approval_binding)
    .bind(manifest_identity_binding)
    .bind(registry_owned_payload_digest)
    .bind(now_ms)
    .execute(&mut ***tx)
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

async fn load_candidate_by_id_tx(
    repository: &SqliteMcpPlatformRepository,
    tx: &mut super::AnchoredTransaction<'_>,
    candidate_id: &str,
) -> McpPlatformResult<Option<IntakeCandidateRecord>> {
    let row = sqlx::query(LATEST_CANDIDATE_SQL)
        .bind(candidate_id)
        .fetch_optional(&mut ***tx)
        .await
        .map_err(map_sqlx)?;
    row.as_ref()
        .map(|row| decode_candidate_row(repository, row))
        .transpose()
}

async fn load_candidate_by_submission_binding_tx(
    repository: &SqliteMcpPlatformRepository,
    tx: &mut super::AnchoredTransaction<'_>,
    submission_binding: &str,
) -> McpPlatformResult<Option<IntakeCandidateRecord>> {
    let row = sqlx::query(LATEST_CANDIDATE_BY_SUBMISSION_SQL)
        .bind(submission_binding)
        .fetch_optional(&mut ***tx)
        .await
        .map_err(map_sqlx)?;
    row.as_ref()
        .map(|row| decode_candidate_row(repository, row))
        .transpose()
}

async fn load_intake_inspection_consent_tx(
    tx: &mut super::AnchoredTransaction<'_>,
    consent_id: &str,
) -> McpPlatformResult<Option<InspectionConsentRecord>> {
    let row = sqlx::query(INTAKE_INSPECTION_CONSENT_SQL)
        .bind(consent_id)
        .fetch_optional(&mut ***tx)
        .await
        .map_err(map_sqlx)?;
    row.as_ref().map(decode_inspection_consent_row).transpose()
}

async fn load_remote_inspection_consent_tx(
    tx: &mut super::AnchoredTransaction<'_>,
    consent_id: &str,
) -> McpPlatformResult<Option<RemoteInspectionConsentRecord>> {
    let row = sqlx::query(REMOTE_INSPECTION_CONSENT_SQL)
        .bind(consent_id)
        .fetch_optional(&mut ***tx)
        .await
        .map_err(map_sqlx)?;
    row.as_ref()
        .map(decode_remote_inspection_consent_row)
        .transpose()
}

async fn load_remote_inspection_attempt_tx(
    tx: &mut super::AnchoredTransaction<'_>,
    attempt_id: &str,
) -> McpPlatformResult<Option<RemoteInspectionAttemptRecord>> {
    let row = sqlx::query(REMOTE_INSPECTION_ATTEMPT_SQL)
        .bind(attempt_id)
        .fetch_optional(&mut ***tx)
        .await
        .map_err(map_sqlx)?;
    row.as_ref()
        .map(decode_remote_inspection_attempt_row)
        .transpose()
}

async fn load_remote_inspection_active_attempt_tx(
    tx: &mut super::AnchoredTransaction<'_>,
    consent_id: &str,
) -> McpPlatformResult<Option<RemoteInspectionAttemptRecord>> {
    let row = sqlx::query(
        r#"SELECT attempt_id,consent_id,attempt_state,claim_nonce,claimed_at_ms,
                  claim_expires_at_ms,owner_started_at_ms,owner_finished_at_ms,finalized_at_ms
           FROM mcp_intake_remote_inspection_attempts
           WHERE consent_id = ? AND attempt_state IN ('claimed','owner_started','owner_finished')
           LIMIT 1"#,
    )
    .bind(consent_id)
    .fetch_optional(&mut ***tx)
    .await
    .map_err(map_sqlx)?;
    row.as_ref()
        .map(decode_remote_inspection_attempt_row)
        .transpose()
}

async fn remote_inspection_consumption_exists_tx(
    tx: &mut super::AnchoredTransaction<'_>,
    consent_id: &str,
) -> McpPlatformResult<bool> {
    sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM mcp_intake_remote_inspection_consumptions WHERE consent_id = ? LIMIT 1",
    )
    .bind(consent_id)
    .fetch_optional(&mut ***tx)
    .await
    .map_err(map_sqlx)
    .map(|value| value.is_some())
}

#[derive(Clone)]
struct RemoteInspectionBindingRow {
    target_handle: String,
    target_identity_binding: String,
    authority_provider: RemoteInspectionAuthorityProvider,
    authority_key_epoch: i64,
    authority_generation: i64,
    authority_record_ref: String,
}

impl RemoteInspectionBindingRow {
    fn has_authority_record_ref(&self) -> bool {
        !self.authority_record_ref.is_empty()
    }
}

fn decode_remote_inspection_binding_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<RemoteInspectionBindingRow> {
    Ok(RemoteInspectionBindingRow {
        target_handle: row.try_get("target_handle").map_err(map_sqlx)?,
        target_identity_binding: row.try_get("target_identity_binding").map_err(map_sqlx)?,
        authority_provider: RemoteInspectionAuthorityProvider::parse(
            &row.try_get::<String, _>("authority_provider_id")
                .map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        authority_key_epoch: row.try_get("authority_key_epoch").map_err(map_sqlx)?,
        authority_generation: row.try_get("authority_generation").map_err(map_sqlx)?,
        authority_record_ref: row.try_get("authority_record_ref").map_err(map_sqlx)?,
    })
}

async fn load_remote_inspection_reservation_tx(
    tx: &mut super::AnchoredTransaction<'_>,
    reservation_id: &str,
    generation: i64,
) -> McpPlatformResult<Option<RemoteInspectionReservationRecord>> {
    let row = sqlx::query(REMOTE_INSPECTION_RESERVATION_SQL)
        .bind(reservation_id)
        .bind(generation)
        .fetch_optional(&mut ***tx)
        .await
        .map_err(map_sqlx)?;
    match row {
        Some(row) => Ok(Some(decode_remote_inspection_reservation_row(&row)?)),
        None => Ok(None),
    }
}

async fn load_remote_inspection_latest_generation_tx(
    tx: &mut super::AnchoredTransaction<'_>,
    reservation_id: &str,
) -> McpPlatformResult<Option<RemoteInspectionReservationRecord>> {
    let generation = sqlx::query_scalar::<_, i64>(
        "SELECT generation FROM mcp_intake_remote_inspection_reservations \
         WHERE reservation_id = ? ORDER BY generation DESC LIMIT 1",
    )
    .bind(reservation_id)
    .fetch_optional(&mut ***tx)
    .await
    .map_err(map_sqlx)?;
    match generation {
        Some(value) => load_remote_inspection_reservation_tx(tx, reservation_id, value).await,
        None => Ok(None),
    }
}

async fn load_remote_inspection_reservation_by_attempt_tx(
    tx: &mut super::AnchoredTransaction<'_>,
    attempt_id: &str,
) -> McpPlatformResult<Option<RemoteInspectionReservationRecord>> {
    let row = sqlx::query(
        "SELECT reservation_id,generation FROM mcp_intake_remote_inspection_reservations \
         WHERE claim_attempt_id = ? ORDER BY generation DESC LIMIT 1",
    )
    .bind(attempt_id)
    .fetch_optional(&mut ***tx)
    .await
    .map_err(map_sqlx)?;
    match row {
        Some(row) => {
            let reservation_id = row
                .try_get::<String, _>("reservation_id")
                .map_err(map_sqlx)?;
            let generation = row.try_get::<i64, _>("generation").map_err(map_sqlx)?;
            load_remote_inspection_reservation_tx(tx, &reservation_id, generation).await
        }
        None => Ok(None),
    }
}

async fn load_remote_inspection_binding_row_tx(
    tx: &mut super::AnchoredTransaction<'_>,
    reservation_id: &str,
    generation: i64,
) -> McpPlatformResult<Option<RemoteInspectionBindingRow>> {
    let row = sqlx::query(REMOTE_INSPECTION_BINDING_SQL)
        .bind(reservation_id)
        .bind(generation)
        .fetch_optional(&mut ***tx)
        .await
        .map_err(map_sqlx)?;
    row.as_ref()
        .map(decode_remote_inspection_binding_row)
        .transpose()
}

async fn load_remote_inspection_binding_row_by_consent_tx(
    tx: &mut super::AnchoredTransaction<'_>,
    consent_id: &str,
) -> McpPlatformResult<Option<RemoteInspectionBindingRow>> {
    let row = sqlx::query(REMOTE_INSPECTION_BINDING_BY_CONSENT_SQL)
        .bind(consent_id)
        .fetch_optional(&mut ***tx)
        .await
        .map_err(map_sqlx)?;
    row.as_ref()
        .map(decode_remote_inspection_binding_row)
        .transpose()
}

async fn append_remote_inspection_event_tx(
    tx: &mut super::AnchoredTransaction<'_>,
    reservation_id: &str,
    generation: i64,
    event_ordinal: i64,
    event_type: RemoteInspectionReservationEventType,
    from_state: RemoteInspectionReservationState,
    to_state: RemoteInspectionReservationState,
    consent_id: Option<&str>,
    claim_attempt_id: Option<&str>,
    proof: Option<&VerifiedNoLiveProof>,
    created_at_ms: i64,
) -> McpPlatformResult<()> {
    sqlx::query(
        r#"INSERT INTO mcp_intake_remote_inspection_reservation_events(
                reservation_id,generation,event_ordinal,event_type,from_state,to_state,
                consent_id,claim_attempt_id,proof_kind,proof_id,proof_hmac,proof_key_epoch,
                proof_observed_at_ms,created_at_ms
           ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
    )
    .bind(reservation_id)
    .bind(generation)
    .bind(event_ordinal)
    .bind(event_type.as_str())
    .bind(from_state.as_str())
    .bind(to_state.as_str())
    .bind(consent_id)
    .bind(claim_attempt_id)
    .bind(proof.map(|value| value.kind().as_str()))
    .bind(proof.map(VerifiedNoLiveProof::proof_id))
    .bind(proof.map(VerifiedNoLiveProof::proof_hmac))
    .bind(proof.map(VerifiedNoLiveProof::proof_key_epoch))
    .bind(proof.map(VerifiedNoLiveProof::observed_at_ms))
    .bind(created_at_ms)
    .execute(&mut ***tx)
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

fn reject_catalog_remote_transport(
    source_facet: IntakeSourceFacet,
    transport: IntakeTransport,
) -> McpPlatformResult<()> {
    if source_facet == IntakeSourceFacet::CatalogPlanning
        || transport == IntakeTransport::CatalogReference
    {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::OperationNotSupported,
            REMOTE_INSPECTION_TRANSPORT_UNSUPPORTED_SUBCODE,
        ));
    }
    Ok(())
}

fn decode_candidate_row(
    repository: &SqliteMcpPlatformRepository,
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<IntakeCandidateRecord> {
    let source_facet = row.try_get::<String, _>("source_facet").map_err(map_sqlx)?;
    let transport = row.try_get::<String, _>("transport").map_err(map_sqlx)?;
    let lifecycle_state = row
        .try_get::<String, _>("lifecycle_state")
        .map_err(map_sqlx)?;
    let gate_reason = row
        .try_get::<Option<String>, _>("gate_reason")
        .map_err(map_sqlx)?;
    let record = IntakeCandidateRecord {
        candidate_id: row.try_get("candidate_id").map_err(map_sqlx)?,
        source_facet: IntakeSourceFacet::parse(&source_facet).ok_or_else(integrity_error)?,
        transport: IntakeTransport::parse(&transport).ok_or_else(integrity_error)?,
        lifecycle_state: IntakeLifecycleState::parse(&lifecycle_state)
            .ok_or_else(integrity_error)?,
        gate_reason: match gate_reason {
            Some(reason) => Some(IntakeGateReason::parse(&reason).ok_or_else(integrity_error)?),
            None => None,
        },
        submission_binding: row.try_get("submission_binding").map_err(map_sqlx)?,
        approval_binding: row.try_get("approval_binding").map_err(map_sqlx)?,
        manifest_identity_binding: row.try_get("manifest_identity_binding").map_err(map_sqlx)?,
        redacted_configuration_ref: row
            .try_get("redacted_configuration_ref")
            .map_err(map_sqlx)?,
        descriptor_digest: row.try_get("descriptor_digest").map_err(map_sqlx)?,
        registry_owned_payload_digest: row
            .try_get("registry_owned_payload_digest")
            .map_err(map_sqlx)?,
        revision: row.try_get("seq").map_err(map_sqlx)?,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
        updated_at_ms: row.try_get("updated_at_ms").map_err(map_sqlx)?,
    };
    let configuration_source_facet = IntakeSourceFacet::parse(
        &row.try_get::<String, _>("configuration_source_facet")
            .map_err(map_sqlx)?,
    )
    .ok_or_else(integrity_error)?;
    if record.source_facet != configuration_source_facet {
        return Err(integrity_error());
    }
    validate_private_reference_state(
        repository,
        configuration_source_facet,
        row.try_get::<Option<String>, _>("configuration_private_reference")
            .map_err(map_sqlx)?
            .as_deref(),
    )?;
    if record.registry_owned_payload_digest.is_some() {
        return Err(integrity_error());
    }
    if record.source_facet == IntakeSourceFacet::ApprovedStdioCandidate
        && record.transport == IntakeTransport::Stdio
        && (record.lifecycle_state == IntakeLifecycleState::BindingReady
            || record.manifest_identity_binding.is_some())
    {
        return Err(integrity_error());
    }
    Ok(record)
}

fn decode_remote_inspection_reservation_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<RemoteInspectionReservationRecord> {
    let record = RemoteInspectionReservationRecord {
        reservation_id: row.try_get("reservation_id").map_err(map_sqlx)?,
        generation: row.try_get("generation").map_err(map_sqlx)?,
        candidate_id: row.try_get("candidate_id").map_err(map_sqlx)?,
        candidate_revision: row.try_get("candidate_revision").map_err(map_sqlx)?,
        candidate_lifecycle_state: IntakeLifecycleState::parse(
            &row.try_get::<String, _>("candidate_lifecycle_state")
                .map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        source_facet: IntakeSourceFacet::parse(
            &row.try_get::<String, _>("source_facet").map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        transport: IntakeTransport::parse(
            &row.try_get::<String, _>("transport").map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        purpose: RemoteInspectionPurpose::parse(
            &row.try_get::<String, _>("purpose").map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        policy_revision: row.try_get("policy_revision").map_err(map_sqlx)?,
        intent_fingerprint: row.try_get("intent_fingerprint").map_err(map_sqlx)?,
        intent_fingerprint_key_id: row.try_get("intent_fingerprint_key_id").map_err(map_sqlx)?,
        state: RemoteInspectionReservationState::parse(
            &row.try_get::<String, _>("state").map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        consent_id: row.try_get("consent_id").map_err(map_sqlx)?,
        claim_attempt_id: row.try_get("claim_attempt_id").map_err(map_sqlx)?,
        authority_provider: row
            .try_get::<Option<String>, _>("authority_provider_id")
            .map_err(map_sqlx)?
            .map(|value| {
                RemoteInspectionAuthorityProvider::parse(&value)
                    .ok_or_else(inspection_integrity_error)
            })
            .transpose()?,
        authority_key_epoch: row.try_get("authority_key_epoch").map_err(map_sqlx)?,
        authority_generation: row.try_get("authority_generation").map_err(map_sqlx)?,
        has_authority_record_ref: row.try_get("has_authority_record_ref").map_err(map_sqlx)?,
        no_live_proof_kind: row
            .try_get::<Option<String>, _>("no_live_proof_kind")
            .map_err(map_sqlx)?
            .map(|value| {
                RemoteInspectionNoLiveProofKind::parse(&value)
                    .ok_or_else(inspection_integrity_error)
            })
            .transpose()?,
        no_live_proof_observed_at_ms: row
            .try_get("no_live_proof_observed_at_ms")
            .map_err(map_sqlx)?,
        last_event_ordinal: row.try_get("last_event_ordinal").map_err(map_sqlx)?,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
        updated_at_ms: row.try_get("updated_at_ms").map_err(map_sqlx)?,
    };
    validate_remote_inspection_reservation(&record)?;
    Ok(record)
}

fn decode_inspection_consent_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<InspectionConsentRecord> {
    let candidate_lifecycle_state = IntakeLifecycleState::parse(
        &row.try_get::<String, _>("candidate_lifecycle_state")
            .map_err(map_sqlx)?,
    )
    .ok_or_else(inspection_integrity_error)?;
    let binding = decode_inspection_binding_tuple(row)?;
    if candidate_lifecycle_state == IntakeLifecycleState::LegacyQuarantine
        || binding.consent_lifecycle != InspectionLifecycle::ConsentGranted
    {
        return Err(inspection_integrity_error());
    }
    Ok(InspectionConsentRecord {
        consent_id: row.try_get("consent_id").map_err(map_sqlx)?,
        candidate_id: row.try_get("candidate_id").map_err(map_sqlx)?,
        candidate_lifecycle_state,
        binding,
        expires_at_ms: row.try_get("expires_at_ms").map_err(map_sqlx)?,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
        consumed_at_ms: row.try_get("consumed_at_ms").map_err(map_sqlx)?,
    })
}

fn decode_inspection_snapshot_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<StoredInspectionSnapshot> {
    let binding = decode_inspection_binding_tuple(row)?;
    let inspection_state = InspectionState::parse(
        &row.try_get::<String, _>("inspection_state")
            .map_err(map_sqlx)?,
    )
    .ok_or_else(inspection_integrity_error)?;
    let observation_surface = ObservationSurface::parse(
        &row.try_get::<String, _>("observation_surface")
            .map_err(map_sqlx)?,
    )
    .ok_or_else(inspection_integrity_error)?;
    let operation_phase = OperationPhase::parse(
        &row.try_get::<String, _>("operation_phase")
            .map_err(map_sqlx)?,
    )
    .ok_or_else(inspection_integrity_error)?;
    let failure_family = row
        .try_get::<Option<String>, _>("failure_family")
        .map_err(map_sqlx)?
        .map(|value| InspectionFailureFamily::parse(&value).ok_or_else(inspection_integrity_error))
        .transpose()?;
    let summary_code =
        InspectionSummaryCode::parse(&row.try_get::<String, _>("summary_code").map_err(map_sqlx)?)
            .ok_or_else(inspection_integrity_error)?;
    let conflict_codes = decode::<Vec<InspectionConflictCode>>(
        &row.try_get::<String, _>("conflict_codes_json")
            .map_err(map_sqlx)?,
    )
    .map_err(|_| inspection_integrity_error())?;
    let fact_codes = decode::<Vec<InspectionFactCode>>(
        &row.try_get::<String, _>("fact_codes_json")
            .map_err(map_sqlx)?,
    )
    .map_err(|_| inspection_integrity_error())?;
    let snapshot = InspectionSnapshot {
        snapshot_id: row.try_get("snapshot_id").map_err(map_sqlx)?,
        candidate_id: row.try_get("candidate_id").map_err(map_sqlx)?,
        consent_id: row.try_get("consent_id").map_err(map_sqlx)?,
        binding,
        inspection_state,
        observation_surface,
        operation_phase,
        failure_family,
        summary_code,
        conflict_codes,
        fact_codes,
        reusable: row.try_get("reusable").map_err(map_sqlx)?,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
    };
    validate_snapshot_state(&snapshot)?;
    Ok(snapshot)
}

fn decode_remote_inspection_consent_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<RemoteInspectionConsentRecord> {
    let record = RemoteInspectionConsentRecord {
        consent_id: row.try_get("consent_id").map_err(map_sqlx)?,
        candidate_id: row.try_get("candidate_id").map_err(map_sqlx)?,
        candidate_revision: row.try_get("candidate_revision").map_err(map_sqlx)?,
        candidate_lifecycle_state: IntakeLifecycleState::parse(
            &row.try_get::<String, _>("candidate_lifecycle_state")
                .map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        source_facet: IntakeSourceFacet::parse(
            &row.try_get::<String, _>("source_facet").map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        transport: IntakeTransport::parse(
            &row.try_get::<String, _>("transport").map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        purpose: parse_v25_persistence_purpose(
            &row.try_get::<String, _>("purpose").map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        policy_revision: row.try_get("policy_revision").map_err(map_sqlx)?,
        expires_at_ms: row.try_get("expires_at_ms").map_err(map_sqlx)?,
        authority_provider: RemoteInspectionAuthorityProvider::parse(
            &row.try_get::<String, _>("authority_provider_id")
                .map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        provider_key_epoch: row.try_get("provider_key_epoch").map_err(map_sqlx)?,
        generation: row.try_get("generation").map_err(map_sqlx)?,
        has_authority_record_ref: row.try_get("has_authority_record_ref").map_err(map_sqlx)?,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
    };
    validate_remote_inspection_record(&record)?;
    Ok(record)
}

fn decode_remote_inspection_attempt_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<RemoteInspectionAttemptRecord> {
    let record = RemoteInspectionAttemptRecord {
        attempt_id: row.try_get("attempt_id").map_err(map_sqlx)?,
        consent_id: row.try_get("consent_id").map_err(map_sqlx)?,
        state: RemoteInspectionAttemptState::parse(
            &row.try_get::<String, _>("attempt_state")
                .map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        claim_nonce: row.try_get("claim_nonce").map_err(map_sqlx)?,
        claimed_at_ms: row.try_get("claimed_at_ms").map_err(map_sqlx)?,
        claim_expires_at_ms: row.try_get("claim_expires_at_ms").map_err(map_sqlx)?,
        owner_started_at_ms: row.try_get("owner_started_at_ms").map_err(map_sqlx)?,
        owner_finished_at_ms: row.try_get("owner_finished_at_ms").map_err(map_sqlx)?,
        finalized_at_ms: row.try_get("finalized_at_ms").map_err(map_sqlx)?,
    };
    validate_remote_inspection_attempt_record(&record)?;
    Ok(record)
}

fn decode_remote_inspection_snapshot_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<RemoteInspectionSnapshotRecord> {
    let snapshot = RemoteInspectionSnapshotRecord {
        snapshot_id: row.try_get("snapshot_id").map_err(map_sqlx)?,
        consent_id: row.try_get("consent_id").map_err(map_sqlx)?,
        attempt_id: row.try_get("attempt_id").map_err(map_sqlx)?,
        candidate_id: row.try_get("candidate_id").map_err(map_sqlx)?,
        candidate_revision: row.try_get("candidate_revision").map_err(map_sqlx)?,
        source_facet: IntakeSourceFacet::parse(
            &row.try_get::<String, _>("source_facet").map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        transport: IntakeTransport::parse(
            &row.try_get::<String, _>("transport").map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        purpose: parse_v25_persistence_purpose(
            &row.try_get::<String, _>("purpose").map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        policy_revision: row.try_get("policy_revision").map_err(map_sqlx)?,
        authority_provider: RemoteInspectionAuthorityProvider::parse(
            &row.try_get::<String, _>("authority_provider_id")
                .map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        provider_key_epoch: row.try_get("provider_key_epoch").map_err(map_sqlx)?,
        generation: row.try_get("generation").map_err(map_sqlx)?,
        has_authority_record_ref: row.try_get("has_authority_record_ref").map_err(map_sqlx)?,
        safe_subcode: RemoteInspectionSafeSubcode::parse(
            &row.try_get::<String, _>("safe_subcode").map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        safe_summary: RemoteInspectionSafeSummary::parse(
            &row.try_get::<String, _>("safe_summary").map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        fail_closed: row.try_get("fail_closed").map_err(map_sqlx)?,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
    };
    validate_remote_inspection_snapshot(&snapshot).map_err(|failure| failure.as_mcp_error())?;
    Ok(snapshot)
}

fn decode_remote_inspection_consumption_row(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<RemoteInspectionConsumptionRecord> {
    let record = RemoteInspectionConsumptionRecord {
        consent_id: row.try_get("consent_id").map_err(map_sqlx)?,
        attempt_id: row.try_get("attempt_id").map_err(map_sqlx)?,
        snapshot_id: row.try_get("snapshot_id").map_err(map_sqlx)?,
        candidate_id: row.try_get("candidate_id").map_err(map_sqlx)?,
        candidate_revision: row.try_get("candidate_revision").map_err(map_sqlx)?,
        created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
    };
    if record.candidate_revision <= 0 || record.created_at_ms < 0 {
        return Err(inspection_integrity_error());
    }
    Ok(record)
}

fn decode_inspection_binding_tuple(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<InspectionBindingTuple> {
    let binding = InspectionBindingTuple {
        candidate_revision: row.try_get("candidate_revision").map_err(map_sqlx)?,
        source_facet: IntakeSourceFacet::parse(
            &row.try_get::<String, _>("source_facet").map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        transport: IntakeTransport::parse(
            &row.try_get::<String, _>("transport").map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        purpose: InspectionPurpose::parse(&row.try_get::<String, _>("purpose").map_err(map_sqlx)?)
            .ok_or_else(inspection_integrity_error)?,
        consent_binding: row.try_get("consent_binding").map_err(map_sqlx)?,
        consent_lifecycle: InspectionLifecycle::parse(
            &row.try_get::<String, _>("consent_lifecycle")
                .map_err(map_sqlx)?,
        )
        .ok_or_else(inspection_integrity_error)?,
        policy_revision: row.try_get("policy_revision").map_err(map_sqlx)?,
    };
    validate_stored_inspection_binding(&binding)?;
    Ok(binding)
}

fn validate_remote_inspection_record(
    record: &RemoteInspectionConsentRecord,
) -> McpPlatformResult<()> {
    validate_remote_inspection_consent_record(record)
}

fn validate_remote_inspection_attempt_record(
    record: &RemoteInspectionAttemptRecord,
) -> McpPlatformResult<()> {
    crate::mcp_platform::intake_remote_inspection::validate_remote_inspection_attempt_record(record)
}

fn validate_remote_snapshot_against_consent(
    snapshot: &RemoteInspectionSnapshotRecord,
    consent: &RemoteInspectionConsentRecord,
    attempt: &RemoteInspectionAttemptRecord,
) -> McpPlatformResult<()> {
    if attempt.consent_id != consent.consent_id
        || snapshot.consent_id != consent.consent_id
        || snapshot.attempt_id != attempt.attempt_id
        || snapshot.candidate_id != consent.candidate_id
        || snapshot.candidate_revision != consent.candidate_revision
        || snapshot.source_facet != consent.source_facet
        || snapshot.transport != consent.transport
        || snapshot.purpose != consent.purpose
        || snapshot.policy_revision != consent.policy_revision
        || snapshot.authority_provider != consent.authority_provider
        || snapshot.provider_key_epoch != consent.provider_key_epoch
        || snapshot.generation != consent.generation
        || snapshot.has_authority_record_ref != consent.has_authority_record_ref
        || snapshot.created_at_ms
            < attempt
                .owner_finished_at_ms
                .ok_or_else(inspection_integrity_error)?
    {
        return Err(remote_integrity_failure(
            RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
        ));
    }
    Ok(())
}

fn candidate_state_conflict() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::CandidateStateConflict,
        "candidate state transition is not allowed",
    )
}

fn manifest_identity_conflict() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::ManifestIdentityConflict,
        "candidate manifest identity is already bound to a different candidate",
    )
}

fn idempotency_conflict() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IdempotencyConflict,
        "candidate submission binding already refers to different content",
    )
}

fn integrity_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "stored intake candidate data failed integrity validation",
    )
}

fn inspection_integrity_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "stored intake inspection data failed integrity validation",
    )
}

fn invalid_request() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::InvalidRequest,
        "MCP platform request is invalid",
    )
}

fn unavailable_stdio_payload_binding() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::OperationNotSupported,
        "registry-owned stdio payload binding is unavailable",
    )
}

fn inspection_identity_conflict() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::CandidateStateConflict,
        "inspection consent is bound to a different candidate",
    )
}

fn remote_attempt_failure(subcode: RemoteInspectionSafeSubcode) -> McpPlatformError {
    match subcode {
        RemoteInspectionSafeSubcode::ActiveAttemptExists
        | RemoteInspectionSafeSubcode::ClaimNonceMismatch
        | RemoteInspectionSafeSubcode::InvalidAttemptTransition
        | RemoteInspectionSafeSubcode::ConsentExpired
        | RemoteInspectionSafeSubcode::ConsentConsumed => McpPlatformError::new(
            McpPlatformErrorCode::CandidateStateConflict,
            "remote inspection attempt failed closed",
        ),
        _ => remote_integrity_failure(subcode),
    }
}

fn remote_integrity_failure(subcode: RemoteInspectionSafeSubcode) -> McpPlatformError {
    let _ = subcode;
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "stored remote inspection data failed integrity validation",
    )
}

fn validate_private_reference_input(
    repository: &SqliteMcpPlatformRepository,
    source_facet: IntakeSourceFacet,
    private_reference: Option<&str>,
) -> McpPlatformResult<()> {
    match validate_private_reference_for_source_facet(repository, source_facet, private_reference) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == McpPlatformErrorCode::IntegrityError => {
            Err(invalid_request())
        }
        Err(error) => Err(error),
    }
}

fn validate_private_reference_state(
    repository: &SqliteMcpPlatformRepository,
    source_facet: IntakeSourceFacet,
    private_reference: Option<&str>,
) -> McpPlatformResult<()> {
    validate_private_reference_for_source_facet(repository, source_facet, private_reference)
}

fn validate_private_reference_for_source_facet(
    repository: &SqliteMcpPlatformRepository,
    source_facet: IntakeSourceFacet,
    private_reference: Option<&str>,
) -> McpPlatformResult<()> {
    let Some(kind) = IntakePrivateReferenceKind::for_source_facet(source_facet) else {
        return if private_reference.is_none() {
            Ok(())
        } else {
            Err(integrity_error())
        };
    };
    match private_reference {
        Some(value) => repository.verify_minted_intake_private_reference(kind, value),
        None if !kind.requires_private_reference() => Ok(()),
        None => Err(integrity_error()),
    }
}

fn validate_stored_inspection_binding(binding: &InspectionBindingTuple) -> McpPlatformResult<()> {
    let derived_purpose = InspectionPurpose::derive(binding.source_facet, binding.transport)
        .map_err(|_| inspection_integrity_error())?;
    if binding.candidate_revision <= 0
        || binding.purpose != derived_purpose
        || binding.consent_lifecycle != InspectionLifecycle::ConsentGranted
        || binding.policy_revision
            != trusted_policy_revision_for(binding.purpose)
                .map_err(|_| inspection_integrity_error())?
        || binding.consent_binding.len() != 64
    {
        return Err(inspection_integrity_error());
    }
    Ok(())
}

fn validate_snapshot_state(snapshot: &InspectionSnapshot) -> McpPlatformResult<()> {
    validate_stored_inspection_binding(&snapshot.binding)?;
    validate_snapshot_semantic_integrity(snapshot)
}

fn validate_snapshot_semantic_integrity(snapshot: &InspectionSnapshot) -> McpPlatformResult<()> {
    validate_snapshot_classification(snapshot)?;
    let expected = match snapshot.binding.purpose {
        InspectionPurpose::RemoteCandidateBoundary => build_remote_blocked_snapshot(
            snapshot.snapshot_id.clone(),
            snapshot.candidate_id.clone(),
            snapshot.consent_id.clone(),
            snapshot.binding.clone(),
            snapshot.created_at_ms,
        ),
        InspectionPurpose::ApprovedStdioLocalMetadata => build_stdio_local_snapshot(
            snapshot.snapshot_id.clone(),
            snapshot.candidate_id.clone(),
            snapshot.consent_id.clone(),
            snapshot.binding.clone(),
            snapshot.created_at_ms,
        ),
    };
    if snapshot.inspection_state != expected.inspection_state
        || snapshot.observation_surface != expected.observation_surface
        || snapshot.operation_phase != expected.operation_phase
        || snapshot.failure_family != expected.failure_family
        || snapshot.summary_code != expected.summary_code
        || snapshot.conflict_codes != expected.conflict_codes
        || snapshot.fact_codes != expected.fact_codes
        || snapshot.reusable != expected.reusable
    {
        return Err(inspection_integrity_error());
    }
    Ok(())
}

fn validate_snapshot_classification(snapshot: &InspectionSnapshot) -> McpPlatformResult<()> {
    if snapshot.reusable {
        return Err(inspection_integrity_error());
    }
    match snapshot.summary_code {
        InspectionSummaryCode::LocalMetadataOnly => {
            if snapshot.inspection_state != InspectionState::InspectionRecorded
                || snapshot.failure_family.is_some()
                || !snapshot.conflict_codes.is_empty()
            {
                return Err(inspection_integrity_error());
            }
        }
        _ => {
            if snapshot.conflict_codes.is_empty() {
                return Err(inspection_integrity_error());
            }
            let expected = classify_failure(&snapshot.conflict_codes);
            if snapshot.inspection_state != expected.state
                || snapshot.failure_family != expected.failure_family
                || snapshot.summary_code != expected.summary_code
                || snapshot.reusable != expected.reusable
            {
                return Err(inspection_integrity_error());
            }
        }
    }
    Ok(())
}

fn validate_snapshot_against_current(
    input: &RecordIntakeInspectionSnapshot,
    current: &IntakeCandidateRecord,
    consent: &InspectionConsentRecord,
) -> McpPlatformResult<()> {
    if consent.candidate_id != input.snapshot.candidate_id
        || current.candidate_id != input.snapshot.candidate_id
    {
        return Err(inspection_identity_conflict());
    }
    if consent.consumed_at_ms.is_some() {
        return Err(candidate_state_conflict());
    }
    if consent.expires_at_ms <= input.snapshot.created_at_ms {
        return Err(candidate_state_conflict());
    }
    if current.revision != input.snapshot.binding.candidate_revision
        || current.revision != consent.binding.candidate_revision
        || current.lifecycle_state != input.candidate_lifecycle_state
        || current.lifecycle_state != consent.candidate_lifecycle_state
        || current.source_facet != input.snapshot.binding.source_facet
        || current.source_facet != consent.binding.source_facet
        || current.transport != input.snapshot.binding.transport
        || current.transport != consent.binding.transport
        || input.snapshot.binding.purpose != consent.binding.purpose
        || input.snapshot.binding.policy_revision != consent.binding.policy_revision
        || input.snapshot.binding.consent_binding != consent.binding.consent_binding
        || input.snapshot.binding.consent_lifecycle != InspectionLifecycle::ConsentGranted
        || consent.binding.consent_lifecycle != InspectionLifecycle::ConsentGranted
    {
        return Err(candidate_state_conflict());
    }
    Ok(())
}

fn intake_private_reference_mac_payload(
    kind: IntakePrivateReferenceKind,
    nonce_hex: &str,
) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"goose.mcp-platform.intake.private-reference.v2");
    payload.push(0);
    payload.extend_from_slice(kind.label().as_bytes());
    payload.push(0xff);
    payload.extend_from_slice(nonce_hex.len().to_string().as_bytes());
    payload.push(0xfe);
    payload.extend_from_slice(nonce_hex.as_bytes());
    payload
}

impl SqliteMcpPlatformRepository {
    fn verify_minted_intake_private_reference(
        &self,
        kind: IntakePrivateReferenceKind,
        value: &str,
    ) -> McpPlatformResult<()> {
        let parsed =
            parse_minted_intake_private_reference(kind, value).ok_or_else(integrity_error)?;
        self.integrity_signer.verify(
            "intake-private-reference",
            &intake_private_reference_mac_payload(kind, parsed.nonce_hex),
            parsed.mac_hex,
        )
    }

    #[cfg(test)]
    pub(crate) async fn tamper_remote_attempt_owner_finished_before_started(
        &self,
        attempt_id: &str,
        owner_finished_at_ms: i64,
    ) -> McpPlatformResult<()> {
        remote_inspection_v15_tamper_fixture::attempt_owner_finished_before_started(
            self,
            attempt_id,
            owner_finished_at_ms,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn rewrite_remote_inspection_schema_to_v25_for_migration_test(
        &self,
    ) -> McpPlatformResult<()> {
        remote_inspection_v15_tamper_fixture::rewrite_schema_to_v25(self).await
    }

    #[cfg(test)]
    pub(crate) async fn tamper_drop_remote_inspection_v26_update_trigger(
        &self,
    ) -> McpPlatformResult<()> {
        remote_inspection_v15_tamper_fixture::drop_v26_update_trigger(self).await
    }

    #[cfg(test)]
    pub(crate) async fn tamper_try_insert_catalog_remote_consent(
        &self,
        consent_id: &str,
        candidate_id: &str,
        candidate_revision: i64,
        created_at_ms: i64,
        expires_at_ms: i64,
    ) -> McpPlatformResult<()> {
        remote_inspection_v15_tamper_fixture::try_insert_catalog_remote_consent(
            self,
            consent_id,
            candidate_id,
            candidate_revision,
            created_at_ms,
            expires_at_ms,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn tamper_try_update_catalog_remote_consent(
        &self,
        consent_id: &str,
    ) -> McpPlatformResult<()> {
        remote_inspection_v15_tamper_fixture::try_update_catalog_remote_consent(self, consent_id)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn tamper_try_update_catalog_remote_snapshot(
        &self,
        snapshot_id: &str,
    ) -> McpPlatformResult<()> {
        remote_inspection_v15_tamper_fixture::try_update_catalog_remote_snapshot(self, snapshot_id)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn tamper_try_update_remote_binding_target_handle(
        &self,
        consent_id: &str,
    ) -> McpPlatformResult<()> {
        remote_inspection_v15_tamper_fixture::try_update_remote_binding_target_handle(
            self, consent_id,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn tamper_try_insert_generation_gap_reservation(
        &self,
        reservation_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        remote_inspection_v15_tamper_fixture::try_insert_generation_gap_reservation(
            self,
            reservation_id,
            now_ms,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn tamper_try_reuse_remote_intent_fingerprint(
        &self,
        reservation_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        remote_inspection_v15_tamper_fixture::try_reuse_remote_intent_fingerprint(
            self,
            reservation_id,
            now_ms,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn tamper_try_insert_mismatched_commit_tuple(
        &self,
        consent_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        remote_inspection_v15_tamper_fixture::try_insert_mismatched_commit_tuple(
            self, consent_id, now_ms,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn tamper_try_prebind_tombstone_wrong_proof_kind(
        &self,
        reservation_id: &str,
        generation: i64,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        remote_inspection_v15_tamper_fixture::try_prebind_tombstone_wrong_proof_kind(
            self,
            reservation_id,
            generation,
            now_ms,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn remote_inspection_table_counts(
        &self,
    ) -> McpPlatformResult<Vec<(String, i64)>> {
        remote_inspection_v15_tamper_fixture::remote_inspection_table_counts(self).await
    }

    #[cfg(test)]
    pub(crate) async fn remote_inspection_reservation_events(
        &self,
        reservation_id: &str,
        generation: i64,
    ) -> McpPlatformResult<Vec<(i64, String, String, String, Option<String>)>> {
        remote_inspection_v15_tamper_fixture::remote_inspection_reservation_events(
            self,
            reservation_id,
            generation,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn remote_inspection_attempt_artifact_counts(
        &self,
        attempt_id: &str,
    ) -> McpPlatformResult<(i64, i64)> {
        remote_inspection_v15_tamper_fixture::remote_inspection_attempt_artifact_counts(
            self, attempt_id,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn remote_inspection_persisted_consent_purpose(
        &self,
        consent_id: &str,
    ) -> McpPlatformResult<String> {
        remote_inspection_v15_tamper_fixture::remote_inspection_persisted_consent_purpose(
            self, consent_id,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn remote_inspection_persisted_snapshot_purpose(
        &self,
        snapshot_id: &str,
    ) -> McpPlatformResult<String> {
        remote_inspection_v15_tamper_fixture::remote_inspection_persisted_snapshot_purpose(
            self,
            snapshot_id,
        )
        .await
    }
}

#[cfg(test)]
mod remote_inspection_v15_tamper_fixture {
    use super::*;

    pub(crate) async fn attempt_owner_finished_before_started(
        repository: &SqliteMcpPlatformRepository,
        attempt_id: &str,
        owner_finished_at_ms: i64,
    ) -> McpPlatformResult<()> {
        sqlx::query(
            "UPDATE mcp_intake_remote_inspection_attempts \
             SET attempt_state = 'owner_finished', owner_finished_at_ms = ? \
             WHERE attempt_id = ?",
        )
        .bind(owner_finished_at_ms)
        .bind(attempt_id)
        .execute(&repository.pool)
        .await
        .map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn rewrite_schema_to_v25(
        repository: &SqliteMcpPlatformRepository,
    ) -> McpPlatformResult<()> {
        let statements = [
            "DROP TRIGGER IF EXISTS mcp_intake_remote_inspection_reservations_validate_update_v26",
            "DROP TRIGGER IF EXISTS mcp_intake_remote_inspection_commit_events_immutable_v26",
            "DROP TRIGGER IF EXISTS mcp_intake_remote_inspection_bindings_immutable_v26",
            "DROP TRIGGER IF EXISTS mcp_intake_remote_inspection_reservation_events_immutable_v26",
            "DROP TRIGGER IF EXISTS mcp_intake_remote_inspection_commit_events_validate_insert_v26",
            "DROP TRIGGER IF EXISTS mcp_intake_remote_inspection_bindings_validate_insert_v26",
            "DROP TRIGGER IF EXISTS mcp_intake_remote_inspection_reservations_validate_insert_v26",
            "DROP TRIGGER IF EXISTS mcp_intake_remote_inspection_snapshots_v26_manual_only_update",
            "DROP TRIGGER IF EXISTS mcp_intake_remote_inspection_snapshots_v26_manual_only",
            "DROP TRIGGER IF EXISTS mcp_intake_remote_inspection_consents_v26_manual_only_update",
            "DROP TRIGGER IF EXISTS mcp_intake_remote_inspection_consents_v26_manual_only",
            "DROP TRIGGER IF EXISTS mcp_intake_remote_inspection_commit_ord2_requires_ord1_v26",
            "DROP TRIGGER IF EXISTS mcp_intake_remote_inspection_reserved_event_v26",
            "DROP TABLE IF EXISTS mcp_intake_remote_inspection_commit_events_v26",
            "DROP TABLE IF EXISTS mcp_intake_remote_inspection_bindings_b26",
            "DROP TABLE IF EXISTS mcp_intake_remote_inspection_reservation_events",
            "DROP TABLE IF EXISTS mcp_intake_remote_inspection_reservations",
            "DROP TABLE IF EXISTS mcp_intake_remote_inspection_scope_lineage_anchors_v26",
            "DELETE FROM schema_version WHERE version = 26",
        ];
        let mut tx = repository.pool.begin().await.map_err(map_sqlx)?;
        for statement in statements {
            sqlx::query(statement)
                .execute(tx.as_mut())
                .await
                .map_err(map_sqlx)?;
        }
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn drop_v26_update_trigger(
        repository: &SqliteMcpPlatformRepository,
    ) -> McpPlatformResult<()> {
        sqlx::query(
            "DROP TRIGGER IF EXISTS mcp_intake_remote_inspection_reservations_validate_update_v26",
        )
        .execute(&repository.pool)
        .await
        .map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn try_insert_catalog_remote_consent(
        repository: &SqliteMcpPlatformRepository,
        consent_id: &str,
        candidate_id: &str,
        candidate_revision: i64,
        created_at_ms: i64,
        expires_at_ms: i64,
    ) -> McpPlatformResult<()> {
        sqlx::query(
            r#"INSERT INTO mcp_intake_remote_inspection_consents(
                    consent_id,candidate_id,candidate_revision,candidate_lifecycle_state,
                    source_facet,transport,purpose,policy_revision,expires_at_ms,target_handle,
                    target_identity_binding,authority_provider_id,provider_key_epoch,generation,
                    created_at_ms
               ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
        )
        .bind(consent_id)
        .bind(candidate_id)
        .bind(candidate_revision)
        .bind("submitted")
        .bind("catalog_planning")
        .bind("catalog_reference")
        .bind("private_remote_check_v25")
        .bind(1_i64)
        .bind(expires_at_ms)
        .bind("catalog_target_handle")
        .bind("catalog_target_binding")
        .bind("sealed_authority")
        .bind(1_i64)
        .bind(1_i64)
        .bind(created_at_ms)
        .execute(&repository.pool)
        .await
        .map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn try_update_catalog_remote_consent(
        repository: &SqliteMcpPlatformRepository,
        consent_id: &str,
    ) -> McpPlatformResult<()> {
        sqlx::query(
            "UPDATE mcp_intake_remote_inspection_consents \
             SET transport = 'catalog_reference' \
             WHERE consent_id = ?",
        )
        .bind(consent_id)
        .execute(&repository.pool)
        .await
        .map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn try_update_catalog_remote_snapshot(
        repository: &SqliteMcpPlatformRepository,
        snapshot_id: &str,
    ) -> McpPlatformResult<()> {
        sqlx::query(
            "UPDATE mcp_intake_remote_inspection_snapshots \
             SET transport = 'catalog_reference' \
             WHERE snapshot_id = ?",
        )
        .bind(snapshot_id)
        .execute(&repository.pool)
        .await
        .map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn try_update_remote_binding_target_handle(
        repository: &SqliteMcpPlatformRepository,
        consent_id: &str,
    ) -> McpPlatformResult<()> {
        sqlx::query(
            "UPDATE mcp_intake_remote_inspection_bindings_b26 \
             SET target_handle = 'tampered_target_handle' \
             WHERE consent_id = ?",
        )
        .bind(consent_id)
        .execute(&repository.pool)
        .await
        .map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn try_insert_generation_gap_reservation(
        repository: &SqliteMcpPlatformRepository,
        reservation_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        sqlx::query(
            r#"INSERT INTO mcp_intake_remote_inspection_reservations(
                    reservation_id,generation,candidate_id,candidate_revision,candidate_lifecycle_state,
                    source_facet,transport,purpose,policy_revision,intent_fingerprint,
                    intent_fingerprint_key_id,state,consent_id,claim_attempt_id,
                    no_live_proof_kind,no_live_proof_id,no_live_proof_hmac,no_live_proof_key_epoch,
                    no_live_proof_observed_at_ms,last_event_ordinal,created_at_ms,updated_at_ms
               )
               SELECT reservation_id,3,candidate_id,candidate_revision,candidate_lifecycle_state,
                      source_facet,transport,purpose,policy_revision,'gap_intent','gap_intent_key',
                      'prebind_reserved',NULL,NULL,NULL,NULL,NULL,NULL,NULL,1,?,?
               FROM mcp_intake_remote_inspection_reservations
               WHERE reservation_id = ? AND generation = 1"#,
        )
        .bind(now_ms)
        .bind(now_ms)
        .bind(reservation_id)
        .execute(&repository.pool)
        .await
        .map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn try_reuse_remote_intent_fingerprint(
        repository: &SqliteMcpPlatformRepository,
        reservation_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        sqlx::query(
            r#"INSERT INTO mcp_intake_remote_inspection_reservations(
                    reservation_id,generation,candidate_id,candidate_revision,candidate_lifecycle_state,
                    source_facet,transport,purpose,policy_revision,intent_fingerprint,
                    intent_fingerprint_key_id,state,consent_id,claim_attempt_id,
                    no_live_proof_kind,no_live_proof_id,no_live_proof_hmac,no_live_proof_key_epoch,
                    no_live_proof_observed_at_ms,last_event_ordinal,created_at_ms,updated_at_ms
               )
               SELECT reservation_id,2,candidate_id,candidate_revision,candidate_lifecycle_state,
                      source_facet,transport,purpose,policy_revision,intent_fingerprint,
                      intent_fingerprint_key_id,'prebind_reserved',NULL,NULL,NULL,NULL,NULL,NULL,
                      NULL,1,?,?
               FROM mcp_intake_remote_inspection_reservations
               WHERE reservation_id = ? AND generation = 1"#,
        )
        .bind(now_ms)
        .bind(now_ms)
        .bind(reservation_id)
        .execute(&repository.pool)
        .await
        .map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn try_insert_mismatched_commit_tuple(
        repository: &SqliteMcpPlatformRepository,
        consent_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        sqlx::query(
            r#"INSERT INTO mcp_intake_remote_inspection_commit_events_v26(
                    consent_id,ordinal,reservation_id,reservation_generation,candidate_id,
                    candidate_revision,source_facet,transport,purpose,policy_revision,
                    intent_fingerprint,intent_fingerprint_key_id,target_handle,
                    target_identity_binding,authority_provider_id,authority_key_epoch,
                    authority_generation,authority_record_ref,commit_state,no_live_proof_kind,
                    no_live_proof_id,no_live_proof_hmac,no_live_proof_key_epoch,
                    no_live_proof_observed_at_ms,created_at_ms
               )
               SELECT consent_id,2,reservation_id,reservation_generation,candidate_id,
                      candidate_revision,source_facet,transport,purpose,policy_revision,
                      intent_fingerprint,intent_fingerprint_key_id,'tampered_target_handle',
                      target_identity_binding,authority_provider_id,authority_key_epoch,
                      authority_generation,authority_record_ref,'committed',NULL,NULL,NULL,NULL,
                      NULL,?
               FROM mcp_intake_remote_inspection_bindings_b26
               WHERE consent_id = ?"#,
        )
        .bind(now_ms)
        .bind(consent_id)
        .execute(&repository.pool)
        .await
        .map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn try_prebind_tombstone_wrong_proof_kind(
        repository: &SqliteMcpPlatformRepository,
        reservation_id: &str,
        generation: i64,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        sqlx::query(
            "UPDATE mcp_intake_remote_inspection_reservations \
             SET state = 'prebind_tombstoned_terminal',
                 no_live_proof_kind = 'definitive_no_live_postbind',
                 no_live_proof_id = 'tampered_wrong_kind',
                 no_live_proof_hmac = ?,
                 no_live_proof_key_epoch = 1,
                 no_live_proof_observed_at_ms = ?,
                 updated_at_ms = ?
             WHERE reservation_id = ? AND generation = ?",
        )
        .bind("a".repeat(64))
        .bind(now_ms)
        .bind(now_ms)
        .bind(reservation_id)
        .bind(generation)
        .execute(&repository.pool)
        .await
        .map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn remote_inspection_table_counts(
        repository: &SqliteMcpPlatformRepository,
    ) -> McpPlatformResult<Vec<(String, i64)>> {
        let mut counts = Vec::new();
        for table in [
            "mcp_intake_remote_inspection_consents",
            "mcp_intake_remote_inspection_attempts",
            "mcp_intake_remote_inspection_snapshots",
            "mcp_intake_remote_inspection_consumptions",
            "mcp_intake_remote_inspection_scope_lineage_anchors_v26",
            "mcp_intake_remote_inspection_reservations",
            "mcp_intake_remote_inspection_reservation_events",
            "mcp_intake_remote_inspection_bindings_b26",
            "mcp_intake_remote_inspection_commit_events_v26",
        ] {
            let count = sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&repository.pool)
                .await
                .map_err(map_sqlx)?;
            counts.push((table.to_string(), count));
        }
        Ok(counts)
    }

    pub(crate) async fn remote_inspection_reservation_events(
        repository: &SqliteMcpPlatformRepository,
        reservation_id: &str,
        generation: i64,
    ) -> McpPlatformResult<Vec<(i64, String, String, String, Option<String>)>> {
        let rows = sqlx::query(
            "SELECT event_ordinal,event_type,from_state,to_state,claim_attempt_id
             FROM mcp_intake_remote_inspection_reservation_events
             WHERE reservation_id = ? AND generation = ?
             ORDER BY event_ordinal",
        )
        .bind(reservation_id)
        .bind(generation)
        .fetch_all(&repository.pool)
        .await
        .map_err(map_sqlx)?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    row.try_get("event_ordinal").map_err(map_sqlx)?,
                    row.try_get("event_type").map_err(map_sqlx)?,
                    row.try_get("from_state").map_err(map_sqlx)?,
                    row.try_get("to_state").map_err(map_sqlx)?,
                    row.try_get("claim_attempt_id").map_err(map_sqlx)?,
                ))
            })
            .collect()
    }

    pub(crate) async fn remote_inspection_attempt_artifact_counts(
        repository: &SqliteMcpPlatformRepository,
        attempt_id: &str,
    ) -> McpPlatformResult<(i64, i64)> {
        let snapshot_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM mcp_intake_remote_inspection_snapshots WHERE attempt_id = ?",
        )
        .bind(attempt_id)
        .fetch_one(&repository.pool)
        .await
        .map_err(map_sqlx)?;
        let consumption_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM mcp_intake_remote_inspection_consumptions WHERE attempt_id = ?",
        )
        .bind(attempt_id)
        .fetch_one(&repository.pool)
        .await
        .map_err(map_sqlx)?;
        Ok((snapshot_count, consumption_count))
    }

    pub(crate) async fn remote_inspection_persisted_consent_purpose(
        repository: &SqliteMcpPlatformRepository,
        consent_id: &str,
    ) -> McpPlatformResult<String> {
        sqlx::query_scalar(
            "SELECT purpose FROM mcp_intake_remote_inspection_consents WHERE consent_id = ?",
        )
        .bind(consent_id)
        .fetch_one(&repository.pool)
        .await
        .map_err(map_sqlx)
    }

    pub(crate) async fn remote_inspection_persisted_snapshot_purpose(
        repository: &SqliteMcpPlatformRepository,
        snapshot_id: &str,
    ) -> McpPlatformResult<String> {
        sqlx::query_scalar(
            "SELECT purpose FROM mcp_intake_remote_inspection_snapshots WHERE snapshot_id = ?",
        )
        .bind(snapshot_id)
        .fetch_one(&repository.pool)
        .await
        .map_err(map_sqlx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_platform::intake::{
        format_minted_intake_private_reference, parse_minted_intake_private_reference,
        IntakeConfigurationDescriptor, IntakeLifecycleState, IntakePrivateReferenceKind,
        IntakeSourceFacet, IntakeTransport,
    };
    use crate::mcp_platform::repository::InMemoryIntegritySigner;

    async fn repository() -> SqliteMcpPlatformRepository {
        SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            InMemoryIntegritySigner::new_for_testing([0x61; 32]),
        )
        .await
        .unwrap()
    }

    fn minted_reference(
        repository: &SqliteMcpPlatformRepository,
        kind: IntakePrivateReferenceKind,
    ) -> String {
        repository.mint_intake_private_reference(kind).unwrap()
    }

    fn forged_reference(kind: IntakePrivateReferenceKind, value: &str) -> String {
        let parsed = parse_minted_intake_private_reference(kind, value).unwrap();
        let mut forged_mac = parsed.mac_hex.to_string();
        let replacement = if forged_mac.ends_with('0') { '1' } else { '0' };
        forged_mac.pop();
        forged_mac.push(replacement);
        format_minted_intake_private_reference(kind, parsed.nonce_hex, &forged_mac)
    }

    #[tokio::test]
    async fn intake_configuration_ref_stores_only_redacted_descriptor_fields() {
        let repository = repository().await;
        let private_reference =
            minted_reference(&repository, IntakePrivateReferenceKind::ManualHttpsEndpoint);
        let configuration_ref = repository
            .save_intake_configuration_ref(SaveIntakeConfigurationRef {
                configuration_ref: "cfg_1",
                descriptor: &IntakeConfigurationDescriptor::ManualHttpsCandidate {
                    display_origin: "https://safe.example.com".to_string(),
                },
                private_reference: Some(private_reference.as_str()),
                descriptor_digest: &"1".repeat(64),
                now_ms: 10,
            })
            .await
            .unwrap();
        assert_eq!(configuration_ref, "cfg_1");
        let row: String = sqlx::query_scalar(
            r#"SELECT configuration_ref || '|' || source_facet || '|' || redacted_descriptor_json || '|' ||
                      COALESCE(private_reference,'')
               FROM mcp_intake_configuration_refs WHERE configuration_ref = 'cfg_1'"#,
        )
        .fetch_one(&repository.pool)
        .await
        .unwrap();
        assert!(row.contains("https://safe.example.com"));
        assert!(!row.contains("/mcp?token="));
        assert!(!row.contains("Bearer "));
    }

    #[tokio::test]
    async fn unknown_intake_state_fails_closed() {
        let repository = repository().await;
        repository
            .save_intake_configuration_ref(SaveIntakeConfigurationRef {
                configuration_ref: "cfg_2",
                descriptor: &IntakeConfigurationDescriptor::LegacyQuarantine {
                    legacy_flow: "manual_plan_create".to_string(),
                },
                private_reference: None,
                descriptor_digest: &"2".repeat(64),
                now_ms: 10,
            })
            .await
            .unwrap();
        repository
            .save_intake_candidate(SaveIntakeCandidate {
                candidate_id: "candidate_1",
                submission_binding: &"a".repeat(64),
                source_facet: IntakeSourceFacet::LegacyQuarantine,
                transport: IntakeTransport::Legacy,
                initial_state: IntakeLifecycleState::LegacyQuarantine,
                redacted_configuration_ref: "cfg_2",
                descriptor_digest: &"2".repeat(64),
                now_ms: 11,
            })
            .await
            .unwrap();
        sqlx::query(
            "UPDATE mcp_intake_candidate_revisions SET lifecycle_state = 'execute_ready' WHERE candidate_id = ? AND seq = 1",
        )
        .bind("candidate_1")
        .execute(&repository.pool)
        .await
        .unwrap();
        let error = repository
            .get_intake_candidate("candidate_1")
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[tokio::test]
    async fn unknown_intake_transport_fails_closed() {
        let repository = repository().await;
        let private_reference =
            minted_reference(&repository, IntakePrivateReferenceKind::CatalogManifest);
        repository
            .save_intake_configuration_ref(SaveIntakeConfigurationRef {
                configuration_ref: "cfg_3",
                descriptor: &IntakeConfigurationDescriptor::CatalogPlanning {
                    source_id: "local_persistence".to_string(),
                    mcp_id: "fixture".to_string(),
                    version: "1.0.0".to_string(),
                },
                private_reference: Some(private_reference.as_str()),
                descriptor_digest: &"3".repeat(64),
                now_ms: 10,
            })
            .await
            .unwrap();
        repository
            .save_intake_candidate(SaveIntakeCandidate {
                candidate_id: "candidate_2",
                submission_binding: &"b".repeat(64),
                source_facet: IntakeSourceFacet::CatalogPlanning,
                transport: IntakeTransport::CatalogReference,
                initial_state: IntakeLifecycleState::Submitted,
                redacted_configuration_ref: "cfg_3",
                descriptor_digest: &"3".repeat(64),
                now_ms: 11,
            })
            .await
            .unwrap();
        sqlx::query("UPDATE mcp_intake_candidates SET transport = 'udp' WHERE candidate_id = ?")
            .bind("candidate_2")
            .execute(&repository.pool)
            .await
            .unwrap();
        let error = repository
            .get_intake_candidate("candidate_2")
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[tokio::test]
    async fn intake_configuration_ref_rejects_non_minted_private_reference() {
        let repository = repository().await;
        let error = repository
            .save_intake_configuration_ref(SaveIntakeConfigurationRef {
                configuration_ref: "cfg_invalid",
                descriptor: &IntakeConfigurationDescriptor::ManualHttpsCandidate {
                    display_origin: "https://safe.example.com".to_string(),
                },
                private_reference: Some("opaque-endpoint-ref"),
                descriptor_digest: &"4".repeat(64),
                now_ms: 10,
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn forged_private_reference_fails_closed() {
        let repository = repository().await;
        let private_reference =
            minted_reference(&repository, IntakePrivateReferenceKind::ManualHttpsEndpoint);
        repository
            .save_intake_configuration_ref(SaveIntakeConfigurationRef {
                configuration_ref: "cfg_4",
                descriptor: &IntakeConfigurationDescriptor::ManualHttpsCandidate {
                    display_origin: "https://safe.example.com".to_string(),
                },
                private_reference: Some(private_reference.as_str()),
                descriptor_digest: &"5".repeat(64),
                now_ms: 10,
            })
            .await
            .unwrap();
        repository
            .save_intake_candidate(SaveIntakeCandidate {
                candidate_id: "candidate_4",
                submission_binding: &"c".repeat(64),
                source_facet: IntakeSourceFacet::ManualHttpsCandidate,
                transport: IntakeTransport::StreamableHttp,
                initial_state: IntakeLifecycleState::AwaitingConsent,
                redacted_configuration_ref: "cfg_4",
                descriptor_digest: &"5".repeat(64),
                now_ms: 11,
            })
            .await
            .unwrap();
        sqlx::query(
            "UPDATE mcp_intake_configuration_refs SET private_reference = 'forged_private_ref' WHERE configuration_ref = ?",
        )
        .bind("cfg_4")
        .execute(&repository.pool)
        .await
        .unwrap();
        let error = repository
            .get_intake_candidate("candidate_4")
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[tokio::test]
    async fn syntactically_valid_forged_private_reference_is_rejected_on_write() {
        let repository = repository().await;
        let minted = minted_reference(&repository, IntakePrivateReferenceKind::ManualHttpsEndpoint);
        let forged = forged_reference(IntakePrivateReferenceKind::ManualHttpsEndpoint, &minted);
        let error = repository
            .save_intake_configuration_ref(SaveIntakeConfigurationRef {
                configuration_ref: "cfg_forged_write",
                descriptor: &IntakeConfigurationDescriptor::ManualHttpsCandidate {
                    display_origin: "https://safe.example.com".to_string(),
                },
                private_reference: Some(forged.as_str()),
                descriptor_digest: &"5".repeat(64),
                now_ms: 10,
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn forged_registry_owned_payload_digest_fails_closed() {
        let repository = repository().await;
        let private_reference =
            minted_reference(&repository, IntakePrivateReferenceKind::CatalogManifest);
        repository
            .save_intake_configuration_ref(SaveIntakeConfigurationRef {
                configuration_ref: "cfg_5",
                descriptor: &IntakeConfigurationDescriptor::CatalogPlanning {
                    source_id: "local_persistence".to_string(),
                    mcp_id: "fixture".to_string(),
                    version: "1.0.0".to_string(),
                },
                private_reference: Some(private_reference.as_str()),
                descriptor_digest: &"6".repeat(64),
                now_ms: 10,
            })
            .await
            .unwrap();
        repository
            .save_intake_candidate(SaveIntakeCandidate {
                candidate_id: "candidate_5",
                submission_binding: &"d".repeat(64),
                source_facet: IntakeSourceFacet::CatalogPlanning,
                transport: IntakeTransport::CatalogReference,
                initial_state: IntakeLifecycleState::Submitted,
                redacted_configuration_ref: "cfg_5",
                descriptor_digest: &"6".repeat(64),
                now_ms: 11,
            })
            .await
            .unwrap();
        sqlx::query(
            "UPDATE mcp_intake_candidate_revisions SET registry_owned_payload_digest = ? WHERE candidate_id = ? AND seq = 1",
        )
        .bind("a".repeat(64))
        .bind("candidate_5")
        .execute(&repository.pool)
        .await
        .unwrap();
        let error = repository
            .get_intake_candidate("candidate_5")
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[tokio::test]
    async fn forged_approved_stdio_binding_ready_state_fails_closed() {
        let repository = repository().await;
        let private_reference = minted_reference(
            &repository,
            IntakePrivateReferenceKind::ApprovedStdioPayload,
        );
        repository
            .save_intake_configuration_ref(SaveIntakeConfigurationRef {
                configuration_ref: "cfg_6",
                descriptor: &IntakeConfigurationDescriptor::ApprovedStdioCandidate {
                    provider_source_ref: "trusted_stdio".to_string(),
                    declared_mcp_id: Some("fixture_stdio".to_string()),
                    declared_version: Some("1.0.0".to_string()),
                },
                private_reference: Some(private_reference.as_str()),
                descriptor_digest: &"7".repeat(64),
                now_ms: 10,
            })
            .await
            .unwrap();
        repository
            .save_intake_candidate(SaveIntakeCandidate {
                candidate_id: "candidate_6",
                submission_binding: &"e".repeat(64),
                source_facet: IntakeSourceFacet::ApprovedStdioCandidate,
                transport: IntakeTransport::Stdio,
                initial_state: IntakeLifecycleState::Submitted,
                redacted_configuration_ref: "cfg_6",
                descriptor_digest: &"7".repeat(64),
                now_ms: 11,
            })
            .await
            .unwrap();
        sqlx::query(
            "UPDATE mcp_intake_candidate_revisions SET lifecycle_state = 'binding_ready', manifest_identity_binding = ? WHERE candidate_id = ? AND seq = 1",
        )
        .bind("f".repeat(64))
        .bind("candidate_6")
        .execute(&repository.pool)
        .await
        .unwrap();
        let error = repository
            .get_intake_candidate("candidate_6")
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[tokio::test]
    async fn minted_private_reference_remains_verifiable_after_reopen_with_same_authority() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("intake-private-reference-reopen.db");
        let signer = InMemoryIntegritySigner::new_for_testing_with_path_binding(
            [0x62; 32],
            super::super::database_path_binding(&path).unwrap(),
        );
        let repository =
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer.clone())
                .await
                .unwrap();
        let private_reference =
            minted_reference(&repository, IntakePrivateReferenceKind::CatalogManifest);
        repository
            .save_intake_configuration_ref(SaveIntakeConfigurationRef {
                configuration_ref: "cfg_reopen",
                descriptor: &IntakeConfigurationDescriptor::CatalogPlanning {
                    source_id: "local_persistence".to_string(),
                    mcp_id: "fixture".to_string(),
                    version: "1.0.0".to_string(),
                },
                private_reference: Some(private_reference.as_str()),
                descriptor_digest: &"8".repeat(64),
                now_ms: 10,
            })
            .await
            .unwrap();
        repository
            .save_intake_candidate(SaveIntakeCandidate {
                candidate_id: "candidate_reopen",
                submission_binding: &"f".repeat(64),
                source_facet: IntakeSourceFacet::CatalogPlanning,
                transport: IntakeTransport::CatalogReference,
                initial_state: IntakeLifecycleState::Submitted,
                redacted_configuration_ref: "cfg_reopen",
                descriptor_digest: &"8".repeat(64),
                now_ms: 11,
            })
            .await
            .unwrap();
        repository.close().await;

        let reopened = SqliteMcpPlatformRepository::open_path_with_integrity_signer(&path, signer)
            .await
            .unwrap();
        let candidate = reopened
            .get_intake_candidate("candidate_reopen")
            .await
            .unwrap();
        assert_eq!(candidate.redacted_configuration_ref, "cfg_reopen");
        reopened.close().await;
    }
}
