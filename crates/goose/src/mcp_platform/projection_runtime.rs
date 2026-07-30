use sha2::{Digest as _, Sha256};

use crate::config::extensions::ExtensionEntry;

use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::lifecycle::{
    observed_projection_digest, CoreTransportProjectionAdapter, LifecyclePorts,
    ProjectionCommitPlan, ProjectionProofSession, TransportProjectionAdapter,
};
use super::repository::SqliteMcpPlatformRepository;
use super::repository::{
    projection_sink_receipt_binding_payload, ConnectionProjectionRecord, ProjectionAuthorityAnchor,
    ProjectionAuthorization, ProjectionMutationRecord, ProjectionRecoveryConfirmationReceipt,
    ProjectionRepositoryIdentity, ProjectionSinkAtomicProof, ProjectionSinkAtomicProofKind,
    ProjectionSinkCommitReceipt, ProjectionWitnessV2, PROJECTION_SINK_COMMIT_RECEIPT_DOMAIN,
    PROJECTION_SINK_RECOVERY_RECEIPT_DOMAIN,
};

#[derive(Debug, Clone)]
pub(crate) struct RuntimeProjectionAuthority {
    repository: SqliteMcpPlatformRepository,
    sink_identity: String,
    proof_session: ProjectionProofSession,
    write_capability: ProjectionWriteCapability,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectionWriteCapability {
    _private: (),
}

impl RuntimeProjectionAuthority {
    fn new(repository: &SqliteMcpPlatformRepository, ports: &LifecyclePorts) -> Self {
        Self {
            repository: repository.clone(),
            sink_identity: format!(
                "{}:{}",
                ports.projection_sink.adapter_id(),
                ports.projection_sink.adapter_version()
            ),
            proof_session: ProjectionProofSession::new(format!(
                "{}:{}",
                ports.projection_sink.adapter_id(),
                ports.projection_sink.adapter_version()
            )),
            write_capability: ProjectionWriteCapability { _private: () },
        }
    }

    pub(crate) fn write_capability(&self) -> &ProjectionWriteCapability {
        &self.write_capability
    }

    pub(crate) fn sink_identity(&self) -> &str {
        &self.sink_identity
    }

    pub(crate) fn commit_plan(
        &self,
        expected: Option<ExtensionEntry>,
        witness: ProjectionWitnessV2,
    ) -> McpPlatformResult<ProjectionCommitPlan> {
        ProjectionCommitPlan::new(expected, witness, self.proof_session.clone())
    }

    pub(crate) fn confirm_existing_plan(
        &self,
        expected: Option<ExtensionEntry>,
        witness: ProjectionWitnessV2,
    ) -> McpPlatformResult<ProjectionCommitPlan> {
        ProjectionCommitPlan::confirm_existing(expected, witness, self.proof_session.clone())
    }

    #[cfg(test)]
    pub(crate) fn test_only_issue_atomic_proof(
        &self,
        witness: &ProjectionWitnessV2,
        kind: ProjectionSinkAtomicProofKind,
        observed_state_digest: String,
        target_state_digest: String,
    ) -> McpPlatformResult<ProjectionSinkAtomicProof> {
        let Some((adapter_id, adapter_version)) = self.sink_identity.split_once(':') else {
            return Err(integrity_error());
        };
        let attestation = self.proof_session.attest(
            witness,
            adapter_id,
            adapter_version,
            kind,
            &observed_state_digest,
            &target_state_digest,
        );
        ProjectionSinkAtomicProof::new(
            adapter_id.to_string(),
            adapter_version.to_string(),
            witness.runtime_id.clone(),
            observed_state_digest,
            target_state_digest,
            kind,
            attestation,
        )
    }

    pub(crate) async fn repository_identity(
        &self,
    ) -> McpPlatformResult<ProjectionRepositoryIdentity> {
        self.repository
            .verified_projection_writer_repository_identity()
            .await
    }

    pub(crate) async fn bind_mutation(
        &self,
        mutation: &ProjectionMutationRecord,
        projection: Option<&ConnectionProjectionRecord>,
    ) -> McpPlatformResult<ProjectionMutationWriterBinding> {
        let repository_identity = self.repository_identity().await?;
        self.bind_mutation_with_repository_identity(repository_identity, mutation, projection)
    }

    pub(crate) fn bind_mutation_with_repository_identity(
        &self,
        repository_identity: ProjectionRepositoryIdentity,
        mutation: &ProjectionMutationRecord,
        projection: Option<&ConnectionProjectionRecord>,
    ) -> McpPlatformResult<ProjectionMutationWriterBinding> {
        let runtime_id = projection
            .map(|record| record.link_key.clone())
            .unwrap_or_else(|| fallback_link_key(&mutation.managed_mcp_id));
        let projection_digest = projection_binding_digest(projection);
        let observed_state_digest =
            expected_observed_state_digest(&self.sink_identity, &runtime_id, mutation, projection)?;
        let witness = ProjectionWitnessV2::new(
            repository_identity,
            self.sink_identity.clone(),
            runtime_id,
            projection_digest,
            mutation.mutation_id,
            mutation.managed_mcp_id.clone(),
            mutation.expected_revision,
            mutation.previous_enabled,
            mutation.desired_enabled,
            observed_state_digest,
        );
        Ok(ProjectionMutationWriterBinding {
            witness,
            commitment: String::new(),
        })
    }

    pub(crate) async fn validates_mutation_binding(
        &self,
        mutation: &ProjectionMutationRecord,
    ) -> bool {
        let Ok(repository_identity) = self.repository_identity().await else {
            return false;
        };
        self.validates_mutation_binding_with_repository_identity(mutation, &repository_identity)
    }

    pub(crate) fn validates_mutation_binding_with_repository_identity(
        &self,
        mutation: &ProjectionMutationRecord,
        repository_identity: &ProjectionRepositoryIdentity,
    ) -> bool {
        let Some(witness) = mutation.witness_v2() else {
            return false;
        };
        if witness.domain != super::repository::PROJECTION_WITNESS_V2_DOMAIN
            || witness.version != super::repository::PROJECTION_WITNESS_V2_VERSION
            || witness.runtime_id.is_empty()
            || witness.projection_digest.len() != 64
            || witness.observed_state_digest.len() != 64
        {
            return false;
        }
        witness.sink_identity == self.sink_identity
            && witness.repository.provider_id == repository_identity.provider_id
            && witness.repository.instance_id == repository_identity.instance_id
            && witness.repository.path_binding == repository_identity.path_binding
            && witness.repository.key_epoch == repository_identity.key_epoch
            && witness.managed_mcp_id == mutation.managed_mcp_id
            && witness.mutation_id == mutation.mutation_id
            && witness.expected_revision == mutation.expected_revision
            && witness.previous_enabled == mutation.previous_enabled
            && witness.desired_enabled == mutation.desired_enabled
    }

    pub(crate) fn mutation_commitment_payload(
        &self,
        mutation: &ProjectionMutationRecord,
    ) -> McpPlatformResult<Vec<u8>> {
        if mutation.witness_v2().is_none() {
            return Err(integrity_error());
        }
        let witness = mutation.witness_v2().ok_or_else(integrity_error)?;
        Ok(canonical_fields(&[
            b"projection-mutation-authorization-v2",
            witness.domain.as_bytes(),
            witness.version.to_string().as_bytes(),
            witness.repository.provider_id.as_bytes(),
            witness.repository.instance_id.as_bytes(),
            witness.repository.path_binding.as_bytes(),
            witness.repository.key_epoch.to_string().as_bytes(),
            witness.sink_identity.as_bytes(),
            witness.runtime_id.as_bytes(),
            witness.projection_digest.as_bytes(),
            witness.mutation_id.to_string().as_bytes(),
            witness.managed_mcp_id.as_bytes(),
            witness.expected_revision.to_string().as_bytes(),
            bool_flag(witness.previous_enabled),
            bool_flag(witness.desired_enabled),
            witness.observed_state_digest.as_bytes(),
        ]))
    }

    pub(crate) fn issue_sink_commit_receipt(
        &self,
        authorization: &ProjectionAuthorization,
        proof: ProjectionSinkAtomicProof,
    ) -> McpPlatformResult<ProjectionSinkCommitReceipt> {
        let witness = authorization.witness_v2()?;
        if proof.runtime_id() != witness.runtime_id
            || proof.observed_state_digest() != witness.observed_state_digest
            || proof.target_state_digest() != witness.observed_state_digest
            || !self
                .proof_session
                .validates(&witness, &self.sink_identity, &proof)
        {
            return Err(integrity_error());
        }
        let payload = projection_sink_receipt_binding_payload(
            PROJECTION_SINK_COMMIT_RECEIPT_DOMAIN,
            &witness,
            authorization.checkpoint(),
            &proof,
        );
        let repository_binding = self
            .repository
            .sign_projection_receipt_binding(PROJECTION_SINK_COMMIT_RECEIPT_DOMAIN, &payload)?;
        ProjectionSinkCommitReceipt::new(
            witness,
            authorization.checkpoint().clone(),
            proof,
            repository_binding,
        )
    }

    pub(crate) fn issue_sink_recovery_confirmation_receipt(
        &self,
        authorization: &ProjectionAuthorization,
        confirmed_state_digest: &str,
        proof: ProjectionSinkAtomicProof,
    ) -> McpPlatformResult<ProjectionRecoveryConfirmationReceipt> {
        let witness = authorization.witness_v2()?;
        if proof.kind() != ProjectionSinkAtomicProofKind::NoopCompareAndSwap
            || proof.runtime_id() != witness.runtime_id
            || proof.observed_state_digest() != confirmed_state_digest
            || proof.target_state_digest() != confirmed_state_digest
            || !self
                .proof_session
                .validates(&witness, &self.sink_identity, &proof)
        {
            return Err(integrity_error());
        }
        let payload = projection_sink_receipt_binding_payload(
            PROJECTION_SINK_RECOVERY_RECEIPT_DOMAIN,
            &witness,
            authorization.checkpoint(),
            &proof,
        );
        let repository_binding = self
            .repository
            .sign_projection_receipt_binding(PROJECTION_SINK_RECOVERY_RECEIPT_DOMAIN, &payload)?;
        ProjectionRecoveryConfirmationReceipt::new(
            witness,
            authorization.checkpoint().clone(),
            proof,
            repository_binding,
        )
    }

    pub(crate) async fn validates_authorization(
        &self,
        authorization: &ProjectionAuthorization,
    ) -> bool {
        self.validates_mutation_binding(authorization.mutation())
            .await
    }

    pub(crate) async fn validates_receipt_identity(
        &self,
        receipt: &ProjectionSinkCommitReceipt,
    ) -> bool {
        self.validates_sink_receipt_identity(
            receipt.witness(),
            receipt.checkpoint(),
            receipt.proof(),
        )
        .await
    }

    pub(crate) async fn validates_recovery_confirmation_identity(
        &self,
        receipt: &ProjectionRecoveryConfirmationReceipt,
    ) -> bool {
        self.validates_sink_receipt_identity(
            receipt.witness(),
            receipt.checkpoint(),
            receipt.proof(),
        )
        .await
    }

    async fn validates_sink_receipt_identity(
        &self,
        witness: &ProjectionWitnessV2,
        checkpoint: &ProjectionAuthorityAnchor,
        proof: &ProjectionSinkAtomicProof,
    ) -> bool {
        let Ok(repository_identity) = self.repository_identity().await else {
            return false;
        };
        let Some((adapter_id, adapter_version)) = self.sink_identity.split_once(':') else {
            return false;
        };
        witness.domain == super::repository::PROJECTION_WITNESS_V2_DOMAIN
            && witness.version == super::repository::PROJECTION_WITNESS_V2_VERSION
            && witness.sink_identity == self.sink_identity
            && witness.repository.provider_id == repository_identity.provider_id
            && witness.repository.instance_id == repository_identity.instance_id
            && witness.repository.path_binding == repository_identity.path_binding
            && witness.repository.key_epoch == repository_identity.key_epoch
            && proof.adapter_id() == adapter_id
            && proof.adapter_version() == adapter_version
            && proof.runtime_id() == witness.runtime_id
            && self
                .proof_session
                .validates(witness, &self.sink_identity, proof)
            && checkpoint.instance_id == repository_identity.instance_id
            && checkpoint.path_binding == repository_identity.path_binding
            && checkpoint.key_epoch == repository_identity.key_epoch
            && witness.projection_digest.len() == 64
            && witness.observed_state_digest.len() == 64
            && proof.observed_state_digest().len() == 64
            && proof.target_state_digest().len() == 64
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectionMutationWriterBinding {
    pub witness: ProjectionWitnessV2,
    pub commitment: String,
}

pub(super) fn bootstrap_trusted_runtime_authority(
    repository: &SqliteMcpPlatformRepository,
    ports: &LifecyclePorts,
) -> RuntimeProjectionAuthority {
    RuntimeProjectionAuthority::new(repository, ports)
}

#[cfg(debug_assertions)]
pub(super) fn bootstrap_debug_runtime_authority(
    repository: &SqliteMcpPlatformRepository,
    ports: &LifecyclePorts,
) -> RuntimeProjectionAuthority {
    RuntimeProjectionAuthority::new(repository, ports)
}

#[cfg(debug_assertions)]
pub(super) fn bootstrap_debug_runner_authority(
    repository: &SqliteMcpPlatformRepository,
    ports: &LifecyclePorts,
) -> RuntimeProjectionAuthority {
    RuntimeProjectionAuthority::new(repository, ports)
}

#[cfg(debug_assertions)]
pub(super) fn bootstrap_debug_repository_authority(
    repository: &SqliteMcpPlatformRepository,
    ports: &LifecyclePorts,
) -> RuntimeProjectionAuthority {
    RuntimeProjectionAuthority::new(repository, ports)
}

fn canonical_fields(fields: &[&[u8]]) -> Vec<u8> {
    let mut payload = Vec::new();
    for field in fields {
        payload.extend_from_slice(&(field.len() as u64).to_be_bytes());
        payload.extend_from_slice(field);
    }
    payload
}

fn bool_flag(value: bool) -> &'static [u8] {
    if value {
        b"1"
    } else {
        b"0"
    }
}

pub(crate) fn projection_binding_digest(projection: Option<&ConnectionProjectionRecord>) -> String {
    projection.map_or_else(absent_projection_digest, |record| {
        record.projection_digest.clone()
    })
}

pub(crate) fn expected_observed_state_digest(
    sink_identity: &str,
    runtime_id: &str,
    mutation: &ProjectionMutationRecord,
    projection: Option<&ConnectionProjectionRecord>,
) -> McpPlatformResult<String> {
    match projection {
        Some(projection) => {
            let config = CoreTransportProjectionAdapter
                .extension_config(&projection.projection, &projection.link_key)?;
            observed_projection_digest(
                sink_identity,
                runtime_id,
                Some(&crate::config::extensions::ExtensionEntry {
                    enabled: mutation.desired_enabled,
                    config,
                }),
            )
        }
        None if !mutation.desired_enabled => {
            observed_projection_digest(sink_identity, runtime_id, None)
        }
        None => Err(integrity_error()),
    }
}

pub(crate) fn authoritative_persisted_projection_entry(
    default_enabled: bool,
    projection: Option<&ConnectionProjectionRecord>,
) -> McpPlatformResult<Option<ExtensionEntry>> {
    match projection {
        Some(projection) => Ok(Some(ExtensionEntry {
            enabled: default_enabled,
            config: CoreTransportProjectionAdapter
                .extension_config(&projection.projection, &projection.link_key)?,
        })),
        None if !default_enabled => Ok(None),
        None => Err(integrity_error()),
    }
}

pub(crate) fn authoritative_persisted_state_digest(
    sink_identity: &str,
    runtime_id: &str,
    default_enabled: bool,
    projection: Option<&ConnectionProjectionRecord>,
) -> McpPlatformResult<String> {
    let expected = authoritative_persisted_projection_entry(default_enabled, projection)?;
    observed_projection_digest(sink_identity, runtime_id, expected.as_ref())
}

fn absent_projection_digest() -> String {
    crate::utils::bytes_to_hex(Sha256::digest(canonical_fields(&[
        b"projection-binding",
        b"absent",
    ])))
}

fn fallback_link_key(managed_mcp_id: &str) -> String {
    managed_mcp_id
        .strip_prefix("managed_")
        .map(|suffix| format!("managed_mcp_{suffix}"))
        .unwrap_or_else(|| managed_mcp_id.to_string())
}

fn integrity_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "projection authority verification failed",
    )
}
