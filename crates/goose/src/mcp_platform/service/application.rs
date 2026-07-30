use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use std::collections::HashMap;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use url::Url;

#[cfg(test)]
use super::port::{
    BindRemoteInspectionPendingCommit, ClaimCommittedRemoteInspectionAttempt,
    FinalizeRemoteInspectionConsumption, RecordRemoteInspectionCommitOutcome,
    RemoteInspectionRepositoryPort, ReserveOrLoadRemoteInspection,
    TombstoneRemoteInspectionPrebind,
};
use crate::mcp_platform::adapters::plan_for_manifest;
use crate::mcp_platform::catalog::{CatalogCompatibility, CompatibilityTarget};
use crate::mcp_platform::domain::ManagedMcpState;
use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::external_distribution::{
    ExternalCapabilitySnapshot, ExternalDistributionPorts,
};
use crate::mcp_platform::https_manifest_fetcher::{
    HttpsManifestFetchResult, HttpsManifestFetcher, ReqwestHttpsManifestTransport,
    TokioHttpsManifestResolver,
};
use crate::mcp_platform::https_manifest_policy::{HttpsManifestFetchPolicy, ValidatedHttpsUrl};
use crate::mcp_platform::lifecycle::{
    ConfigAuthRequirementResolver, ConfigProjectionSink, CoreTransportProjectionAdapter,
    EmptyHostIntegrationAdapter, LifecyclePorts, SafeRegistrationEffectAdapter,
    TransportProjectionAdapter,
};
use crate::mcp_platform::managed_remote::{
    CoreManagedRemoteHttpNetworkPolicy, RemoteHttpNetworkPolicy, UnavailableRemoteHttpNetworkPolicy,
};
use crate::mcp_platform::manifest::{
    parse_manifest, Architecture, Auth, Distribution, ExactVersion, ManifestProof,
    ManifestSourceMetadata, Platform, SourceRef, Transport, VerifiedManifest,
};
use crate::mcp_platform::plan::{
    AdapterIdentity, EffectSummary, InstallationPlan, PlanStep, PlanWarning,
};
use crate::mcp_platform::policy::{evaluate_manifest_policy, PlanOperation, PolicyContext};
use crate::mcp_platform::repository::{
    ApplyGovernedSourceRefresh, ConfirmSourceProvisioning, GovernedCatalogDocumentKind,
    GovernedCatalogDocumentRecord, GovernedCatalogEntryRecord, GovernedCatalogSourceRecord,
    GovernedSourceRefreshResultState, ManifestSourceContext, RecordGovernedSourceRefreshAudit,
    SaveGovernedCatalogEntry, SaveGovernedCatalogImport,
};
use crate::mcp_platform::repository::{
    ConfirmationEvidence, CreateHealthTask, CreateTask, HttpsManifestProvisionRecord,
    HttpsManifestSourceBinding, ManagedInventoryFilter, ManagedMcpInventoryRecord, ManifestRecord,
    PlanRecord, PlanTarget, SavePlan, TaskRecord, TaskTransition,
};
use crate::mcp_platform::runtime_control::{
    ManagedRuntimeAction as HostRuntimeAction, ManagedRuntimeCommand, ManagedRuntimeControlPort,
};
use crate::mcp_platform::source_provisioning::{
    parse_source_provisioning_snapshot, FrozenSourceProvisioningManifest,
    FrozenSourceProvisioningSnapshot, SourceProvisioningCoordinator,
};
use crate::mcp_platform::task::{
    AdapterEvidence, CompensationDescriptor, RollbackEvidence, RollbackStatus, TaskOperation,
    TaskStatus,
};
use crate::mcp_platform::task_runner::TaskRunner;
use crate::mcp_platform::{
    DistributionEffectAdapter, ProductionDistributionEffectAdapter, ProductionHealthCheckAdapter,
    RuntimeCapabilities, RuntimeCapabilitySnapshot, SqliteMcpPlatformRepository,
};

use super::dependencies::{
    Clock, IdGenerator, ManualStdioProvider, SystemClock, UnsupportedManualStdioProvider,
    UuidGenerator,
};
use super::dto::{
    unique_task_ids, CatalogDetail, CatalogListInput, CatalogLocator, CatalogPage,
    CatalogPlanTarget, CatalogSummary, CompatibilitySummary, CredentialStatus, Eligibility,
    EligibilityOutcome, EligibilityReason, EventsResumeInput, EventsResumePage,
    EvidenceUnavailableReason, EvidenceValue, GovernedCatalogDocumentSummary,
    GovernedCatalogSourceSummary, GovernedDirectoryImportInput,
    GovernedDirectoryManifestImportInput, GovernedImportResult, GovernedManifestImportInput,
    GovernedSourceRefreshInput, GovernedSourceRefreshResult, HealthGetInput, HealthRunInput,
    HealthStatus, HttpsManifestConfirmInput, HttpsManifestConfirmResult,
    HttpsManifestPlanReviewInput, HttpsManifestPrepareInput, HttpsManifestPrepareResult,
    HttpsManifestPreview, ImmutableEvidence, InstallConfirmInput, MachinePolicyState,
    ManagedCredentialEnrollmentBeginInput, ManagedCredentialEnrollmentBeginView,
    ManagedCredentialEnrollmentSubmitInput, ManagedCredentialEnrollmentSubmitView, ManagedGetInput,
    ManagedListInput, ManagedMcpDetail, ManagedMcpPage, ManagedMcpSummary, ManagedRuntimeAction,
    ManagedRuntimeControlInput, ManagedSupplyChainSummary, ManualConnectionInput, ManualHttpAuth,
    ManualPlanCreateInput, ManualStdioSourcesPage, PlanCreateInput, PlanIntent, PlanReversibility,
    PlanReview, RecoverySuggestion, RequestContext, RollbackStrategy, SetDefaultEnabledInput,
    SourcePolicyState, SourceProvisionConfirmInput, SourceProvisionConfirmResult,
    SourceProvisionPrepareInput, SourceProvisionPrepareResult, SourceProvisionPreview,
    SourceProvisionRefreshTransport, SourceProvisionTrustBasis, SourceState, TaskCancelInput,
    TaskGetInput, TaskRef, TaskRetryInput, UserDecision, LOCAL_PERSISTED_SOURCE_ID,
};
use super::port::{HttpsManifestSourceFetcher, UnavailableHttpsManifestSourceFetcher};
#[cfg(test)]
use crate::mcp_platform::credential_authority::{
    CredentialHandle, CredentialReadinessBinding, EnrollmentWriterOwner,
};
#[cfg(test)]
use crate::mcp_platform::credential_enrollment::{
    BeginEnrollmentSessionRequest, ConsumeEnrollmentSessionRequest, EnrollmentCoordinator,
    UserActionBinding,
};
#[cfg(test)]
use crate::mcp_platform::credential_runtime_gate::credential_reference_digest;
#[cfg(test)]
use crate::mcp_platform::intake::{
    ApprovedStdioCandidateInput, CatalogPlanningCandidateInput, IntakeCandidateRecord,
    IntakeConfigurationDescriptor, IntakeLifecycleState, IntakePrivateReferenceKind,
    IntakeSourceFacet, IntakeTransport, LegacyQuarantineCandidateInput, ManualHttpsCandidateInput,
    SaveIntakeCandidate, SaveIntakeConfigurationRef,
};
#[cfg(test)]
use crate::mcp_platform::intake_inspection::{
    batch1_consent_tuple, build_remote_blocked_snapshot, build_stdio_local_snapshot,
    InspectionPurpose, RecordIntakeInspectionSnapshot, SaveIntakeInspectionConsent,
};
#[cfg(test)]
use crate::mcp_platform::manifest::trusted_credential_enrollment_schema;
use crate::mcp_platform::manifest::{
    OriginProvenance, SourceImportKind, VerifiedSourceDocumentRef,
};
#[cfg(test)]
use crate::mcp_platform::repository::{
    ManagedCredentialEnrollmentAuthoritySummary, SaveManagedCredentialEnrollment,
};
#[cfg(test)]
use crate::mcp_platform::task_runner::EnrollmentRuntimeBindingResolver;
const DEFAULT_PAGE_SIZE: u16 = 25;
const MAX_PAGE_SIZE: u16 = 100;
const DEFAULT_EVENT_LIMIT: u16 = 50;
const MAX_EVENT_LIMIT: u16 = 200;
const MAX_TASK_FILTERS: usize = 100;
const MAX_ID_LENGTH: usize = 256;
const MAX_QUERY_LENGTH: usize = 256;
const MAX_CURSOR_LENGTH: usize = 1024;

#[derive(Debug, Clone, Copy)]
pub struct McpPlatformServiceOptions {
    pub compatibility_target: CompatibilityTarget,
    pub plan_ttl_ms: i64,
    pub development_mode: bool,
    pub docker_daemon_policy_allowed: bool,
}

impl Default for McpPlatformServiceOptions {
    fn default() -> Self {
        Self {
            compatibility_target: CompatibilityTarget {
                platform: current_platform(),
                arch: current_architecture(),
            },
            plan_ttl_ms: 15 * 60 * 1000,
            development_mode: false,
            docker_daemon_policy_allowed: true,
        }
    }
}

pub struct McpPlatformService {
    pub(crate) repository: Arc<SqliteMcpPlatformRepository>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) ids: Arc<dyn IdGenerator>,
    options: McpPlatformServiceOptions,
    pub(crate) runner: Arc<TaskRunner>,
    auto_worker: bool,
    worker: Mutex<WorkerState>,
    runtime_capabilities: RuntimeCapabilitySnapshot,
    external_capabilities: ExternalCapabilitySnapshot,
    development_mode: bool,
    manual_stdio_provider: Arc<dyn ManualStdioProvider>,
    remote_http_network_policy: Arc<dyn RemoteHttpNetworkPolicy>,
    governed_source_refresh_fetcher: Arc<dyn GovernedSourceRefreshFetcher>,
    https_manifest_fetcher: Arc<dyn HttpsManifestSourceFetcher>,
    #[cfg(feature = "integration-test-support")]
    https_manifest_confirm_stages: Mutex<HashMap<String, HttpsManifestConfirmStage>>,
    source_provisioning: SourceProvisioningCoordinator,
    #[cfg(test)]
    credential_enrollment: EnrollmentCoordinator,
    #[cfg(test)]
    credential_writer_owner: EnrollmentWriterOwner,
    #[cfg(test)]
    remote_inspection_repository: Arc<dyn RemoteInspectionRepositoryPort>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HttpsManifestConfirmStage {
    Claim,
    RecordUrl,
    Refetch,
    Parse,
    EvidenceRequestedUrl,
    EvidenceFinalUrl,
    EvidenceRaw,
    EvidenceParsed,
    EvidenceRedirect,
    EvidenceDns,
    Policy,
    Finalize,
    Success,
}

impl HttpsManifestConfirmStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::RecordUrl => "record_url",
            Self::Refetch => "refetch",
            Self::Parse => "parse",
            Self::EvidenceRequestedUrl => "evidence_requested_url",
            Self::EvidenceFinalUrl => "evidence_final_url",
            Self::EvidenceRaw => "evidence_raw",
            Self::EvidenceParsed => "evidence_parsed",
            Self::EvidenceRedirect => "evidence_redirect",
            Self::EvidenceDns => "evidence_dns",
            Self::Policy => "policy",
            Self::Finalize => "finalize",
            Self::Success => "success",
        }
    }
}

#[async_trait]
pub trait GovernedSourceRefreshFetcher: Send + Sync {
    async fn fetch(&self, endpoint: &str) -> McpPlatformResult<FrozenSourceProvisioningSnapshot>;
}

struct ProductionHttpsManifestSourceFetcher {
    fetcher: HttpsManifestFetcher<TokioHttpsManifestResolver, ReqwestHttpsManifestTransport>,
}

impl ProductionHttpsManifestSourceFetcher {
    fn new() -> Self {
        Self {
            fetcher: HttpsManifestFetcher::new(
                TokioHttpsManifestResolver,
                ReqwestHttpsManifestTransport,
                HttpsManifestFetchPolicy::default(),
            ),
        }
    }
}

#[async_trait]
impl HttpsManifestSourceFetcher for ProductionHttpsManifestSourceFetcher {
    async fn fetch(&self, requested_url: &str) -> McpPlatformResult<HttpsManifestFetchResult> {
        self.fetcher
            .fetch(requested_url)
            .await
            .map_err(|_| invalid_request())
    }
}

pub struct RemoteGovernedSourceRefreshFetcher {
    policy: Arc<dyn RemoteHttpNetworkPolicy>,
}

impl RemoteGovernedSourceRefreshFetcher {
    fn new(policy: Arc<dyn RemoteHttpNetworkPolicy>) -> Self {
        Self { policy }
    }
}

#[async_trait]
impl GovernedSourceRefreshFetcher for RemoteGovernedSourceRefreshFetcher {
    async fn fetch(&self, endpoint: &str) -> McpPlatformResult<FrozenSourceProvisioningSnapshot> {
        let client = self
            .policy
            .secure_client(endpoint, Duration::from_secs(10))
            .await?;
        let response = client
            .client()
            .get(client.endpoint().clone())
            .send()
            .await
            .map_err(|_| invalid_request())?;
        if !response.status().is_success() {
            return Err(invalid_request());
        }
        let body = response.bytes().await.map_err(|_| invalid_request())?;
        if body.len() > 32 * 1024 * 1024 {
            return Err(invalid_request());
        }
        let wire: GovernedSourceRefreshWire =
            serde_json::from_slice(&body).map_err(|_| invalid_request())?;
        wire.into_snapshot()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GovernedSourceRefreshWire {
    descriptor_b64u: String,
    source_document_b64u: String,
    manifests: Vec<GovernedSourceRefreshManifestWire>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GovernedSourceRefreshManifestWire {
    release_id: String,
    document_b64u: String,
}

impl GovernedSourceRefreshWire {
    fn into_snapshot(self) -> McpPlatformResult<FrozenSourceProvisioningSnapshot> {
        let decode = |value: String| URL_SAFE_NO_PAD.decode(value).map_err(|_| invalid_request());
        Ok(FrozenSourceProvisioningSnapshot {
            descriptor_bytes: decode(self.descriptor_b64u)?,
            source_document_bytes: decode(self.source_document_b64u)?,
            manifests: self
                .manifests
                .into_iter()
                .map(|manifest| {
                    Ok(FrozenSourceProvisioningManifest {
                        release_id: manifest.release_id,
                        document_bytes: decode(manifest.document_b64u)?,
                    })
                })
                .collect::<McpPlatformResult<Vec<_>>>()?,
        })
    }
}

enum WorkerState {
    NotStarted,
    Running(tokio::task::JoinHandle<()>),
    Stopped,
}

impl McpPlatformService {
    pub(crate) fn managed_remote_http_network_policy(&self) -> Arc<dyn RemoteHttpNetworkPolicy> {
        self.remote_http_network_policy.clone()
    }

    pub(crate) async fn https_manifest_prepare(
        &self,
        context: &RequestContext,
        input: HttpsManifestPrepareInput,
    ) -> McpPlatformResult<HttpsManifestPrepareResult> {
        let binding = context
            .transport_session_binding()
            .filter(|value| !value.is_empty())
            .ok_or_else(invalid_request)?;
        let canonical_url = canonical_https_url(&input.url)?;
        let fetched = self
            .https_manifest_fetcher
            .fetch(&canonical_url)
            .await
            .map_err(|_| manifest_external_error())?;
        let verified = parse_manifest(&fetched.raw_bytes).map_err(|_| manifest_external_error())?;
        ensure_import_eligible(
            self.eligibility_for_manifest(&verified, crate::mcp_platform::TrustTier::Local),
        )
        .map_err(|_| manifest_external_error())?;
        let token = format!("https_manifest_confirmation_{}", self.ids.next_id("token"));
        let token_hash = sha256_hex(token.as_bytes());
        let now_ms = self.clock.now_ms();
        let expires_at_ms = now_ms.saturating_add(self.options.plan_ttl_ms);
        let record = HttpsManifestProvisionRecord {
            provision_id: self.ids.next_id("https_manifest_provision"),
            actor: context.actor().to_string(),
            transport_session_binding: binding.to_string(),
            requested_url: Some(canonical_url.clone()),
            requested_url_id: sha256_hex(canonical_url.as_bytes()),
            final_url_id: sha256_hex(fetched.final_url.request_url().as_str().as_bytes()),
            raw_digest: sha256_hex(&fetched.raw_bytes),
            parsed_digest: verified.digest().to_string(),
            redirect_chain_digest: sha256_hex(&fetched.redirect_chain_digest),
            dns_evidence_digest: Some(sha256_hex(&fetched.dns_evidence_digest)),
            frozen_bytes: fetched.raw_bytes.clone(),
            expires_at_ms,
            status: "saved".to_string(),
            created_at_ms: now_ms,
        };
        let provision_id = record.provision_id.clone();
        self.repository
            .save_https_manifest_provision(record, &token_hash)
            .await
            .map_err(|_| manifest_external_error())?;
        Ok(HttpsManifestPrepareResult {
            provision_id,
            server_token: token,
            expires_at_ms,
            preview: https_manifest_preview(&fetched, &verified),
        })
    }

    pub(crate) async fn https_manifest_confirm(
        &self,
        context: &RequestContext,
        input: HttpsManifestConfirmInput,
    ) -> McpPlatformResult<HttpsManifestConfirmResult> {
        let binding = context
            .transport_session_binding()
            .filter(|value| !value.is_empty())
            .ok_or_else(invalid_request)?;
        let token_hash = sha256_hex(input.token.as_bytes());
        let now_ms = self.clock.now_ms();
        self.record_https_manifest_confirm_stage(
            &input.provision_id,
            HttpsManifestConfirmStage::Claim,
        );
        let record = self
            .repository
            .claim_https_manifest_provision(
                &input.provision_id,
                &token_hash,
                context.actor(),
                binding,
                now_ms,
            )
            .await
            .map_err(|_| manifest_external_error())?;
        if !input.confirm {
            self.repository
                .reject_https_manifest_provision(
                    &input.provision_id,
                    &token_hash,
                    context.actor(),
                    binding,
                    now_ms,
                )
                .await
                .map_err(|_| manifest_rejection_error())?;
            return Err(invalid_request());
        }
        self.record_https_manifest_confirm_stage(
            &input.provision_id,
            HttpsManifestConfirmStage::RecordUrl,
        );
        let requested_url = match record
            .requested_url
            .as_deref()
            .and_then(canonical_https_url_for_record)
        {
            Some(value) => value,
            None => {
                return Err(self
                    .reject_after_claim(&input, &token_hash, context, binding, now_ms)
                    .await);
            }
        };
        self.record_https_manifest_confirm_stage(
            &input.provision_id,
            HttpsManifestConfirmStage::Refetch,
        );
        let fetched = match self.https_manifest_fetcher.fetch(&requested_url).await {
            Ok(value) => value,
            Err(_) => {
                return Err(self
                    .reject_after_claim(&input, &token_hash, context, binding, now_ms)
                    .await);
            }
        };
        self.record_https_manifest_confirm_stage(
            &input.provision_id,
            HttpsManifestConfirmStage::Parse,
        );
        let verified = match parse_manifest(&fetched.raw_bytes) {
            Ok(value) => value,
            Err(_) => {
                return Err(self
                    .reject_after_claim(&input, &token_hash, context, binding, now_ms)
                    .await);
            }
        };
        self.record_https_manifest_confirm_stage(
            &input.provision_id,
            HttpsManifestConfirmStage::EvidenceRequestedUrl,
        );
        let requested_url_same = sha256_hex(requested_url.as_bytes()) == record.requested_url_id;
        self.record_https_manifest_confirm_stage(
            &input.provision_id,
            HttpsManifestConfirmStage::EvidenceFinalUrl,
        );
        let final_url_same =
            sha256_hex(fetched.final_url.request_url().as_str().as_bytes()) == record.final_url_id;
        self.record_https_manifest_confirm_stage(
            &input.provision_id,
            HttpsManifestConfirmStage::EvidenceRaw,
        );
        let raw_same = sha256_hex(&fetched.raw_bytes) == record.raw_digest;
        self.record_https_manifest_confirm_stage(
            &input.provision_id,
            HttpsManifestConfirmStage::EvidenceParsed,
        );
        let parsed_same = verified.digest() == record.parsed_digest;
        self.record_https_manifest_confirm_stage(
            &input.provision_id,
            HttpsManifestConfirmStage::EvidenceRedirect,
        );
        let redirect_same =
            sha256_hex(&fetched.redirect_chain_digest) == record.redirect_chain_digest;
        self.record_https_manifest_confirm_stage(
            &input.provision_id,
            HttpsManifestConfirmStage::EvidenceDns,
        );
        let dns_same = Some(sha256_hex(&fetched.dns_evidence_digest)) == record.dns_evidence_digest;
        let evidence_stage = if !requested_url_same {
            Some(HttpsManifestConfirmStage::EvidenceRequestedUrl)
        } else if !final_url_same {
            Some(HttpsManifestConfirmStage::EvidenceFinalUrl)
        } else if !raw_same {
            Some(HttpsManifestConfirmStage::EvidenceRaw)
        } else if !parsed_same {
            Some(HttpsManifestConfirmStage::EvidenceParsed)
        } else if !redirect_same {
            Some(HttpsManifestConfirmStage::EvidenceRedirect)
        } else if !dns_same {
            Some(HttpsManifestConfirmStage::EvidenceDns)
        } else {
            None
        };
        if evidence_stage.is_some() {
            self.record_https_manifest_confirm_stage(
                &input.provision_id,
                evidence_stage.expect("evidence stage is present"),
            );
            return Err(self
                .reject_after_claim(&input, &token_hash, context, binding, now_ms)
                .await);
        }
        self.record_https_manifest_confirm_stage(
            &input.provision_id,
            HttpsManifestConfirmStage::Policy,
        );
        if self
            .eligibility_for_verified_https_manifest(&verified)
            .outcome
            != EligibilityOutcome::Allowed
        {
            return Err(self
                .reject_after_claim(&input, &token_hash, context, binding, now_ms)
                .await);
        }
        let manifest = ManifestRecord {
            verified: verified.clone(),
            proof: ManifestProof::LocalBytes,
            trust_tier: crate::mcp_platform::TrustTier::Local,
            source_metadata: ManifestSourceMetadata {
                source_ref: SourceRef::HttpsManifestUrl {
                    manifest_url: requested_url,
                },
                import_kind: SourceImportKind::HttpsManifestUrl,
                ..ManifestSourceMetadata::default()
            },
            created_at_ms: now_ms,
        };
        self.record_https_manifest_confirm_stage(
            &input.provision_id,
            HttpsManifestConfirmStage::Finalize,
        );
        if self
            .repository
            .finalize_https_manifest_provision(
                &input.provision_id,
                &token_hash,
                context.actor(),
                binding,
                now_ms,
                &manifest,
            )
            .await
            .is_err()
        {
            return Err(self
                .reject_after_claim(&input, &token_hash, context, binding, now_ms)
                .await);
        }
        self.record_https_manifest_confirm_stage(
            &input.provision_id,
            HttpsManifestConfirmStage::Success,
        );
        Ok(HttpsManifestConfirmResult {
            provision_id: input.provision_id,
            confirmed_at_ms: now_ms,
            manifest_digest: verified.digest().to_string(),
            preview: https_manifest_preview(&fetched, &verified),
        })
    }

    pub(crate) async fn https_manifest_plan_review(
        &self,
        context: &RequestContext,
        input: HttpsManifestPlanReviewInput,
    ) -> McpPlatformResult<PlanReview> {
        let binding = context
            .transport_session_binding()
            .filter(|value| !value.is_empty())
            .ok_or_else(invalid_request)?;
        validate_identifier(&input.provision_id)?;
        validate_digest(&input.expected_manifest_digest)?;
        validate_idempotency_key(&input.idempotency_key)?;

        let record = self
            .repository
            .get_consumed_https_manifest_provision(&input.provision_id, context.actor(), binding)
            .await
            .map_err(|_| manifest_external_error())?;
        if record.status != "consumed"
            || record.actor != context.actor()
            || record.transport_session_binding != binding
        {
            return Err(manifest_external_error());
        }
        let requested_url = record
            .requested_url
            .as_deref()
            .and_then(canonical_https_url_for_record)
            .ok_or_else(manifest_external_error)?;
        if sha256_hex(requested_url.as_bytes()) != record.requested_url_id
            || record.final_url_id.is_empty()
            || record.redirect_chain_digest.is_empty()
            || record
                .dns_evidence_digest
                .as_deref()
                .is_none_or(|value| value.is_empty())
            || record.parsed_digest != input.expected_manifest_digest
            || sha256_hex(&record.frozen_bytes) != record.raw_digest
        {
            return Err(manifest_external_error());
        }
        let verified =
            parse_manifest(&record.frozen_bytes).map_err(|_| manifest_external_error())?;
        if verified.digest() != record.parsed_digest {
            return Err(manifest_external_error());
        }
        let manifest = ManifestRecord {
            verified,
            proof: ManifestProof::LocalBytes,
            trust_tier: crate::mcp_platform::TrustTier::Local,
            source_metadata: ManifestSourceMetadata {
                source_ref: SourceRef::HttpsManifestUrl {
                    manifest_url: requested_url,
                },
                import_kind: crate::mcp_platform::manifest::SourceImportKind::HttpsManifestUrl,
                ..ManifestSourceMetadata::default()
            },
            created_at_ms: record.created_at_ms,
        };
        let source_context = ManifestSourceContext::HttpsProvision {
            binding: HttpsManifestSourceBinding {
                provision_id: record.provision_id.clone(),
                requested_url_id: record.requested_url_id.clone(),
                final_url_id: record.final_url_id.clone(),
                redirect_chain_digest: record.redirect_chain_digest.clone(),
                dns_evidence_digest: record
                    .dns_evidence_digest
                    .clone()
                    .ok_or_else(manifest_external_error)?,
                raw_digest: record.raw_digest.clone(),
                parsed_digest: record.parsed_digest.clone(),
            },
        };
        self.plan_create_with_trusted_manifest(
            context,
            PlanCreateInput {
                intent: PlanIntent::Register {
                    manifest_digest: input.expected_manifest_digest,
                    installation_scope: super::dto::InstallationScope::User,
                },
                idempotency_key: input.idempotency_key,
            },
            Some((manifest, source_context)),
        )
        .await
    }

    async fn reject_after_claim(
        &self,
        input: &HttpsManifestConfirmInput,
        token_hash: &str,
        context: &RequestContext,
        binding: &str,
        now_ms: i64,
    ) -> McpPlatformError {
        match self
            .repository
            .reject_https_manifest_provision(
                &input.provision_id,
                token_hash,
                context.actor(),
                binding,
                now_ms,
            )
            .await
        {
            Ok(()) => manifest_external_error(),
            Err(_) => manifest_rejection_error(),
        }
    }

    pub async fn governed_manifest_import(
        &self,
        _context: &RequestContext,
        input: GovernedManifestImportInput,
    ) -> McpPlatformResult<GovernedImportResult> {
        self.require_mutation_authority()?;
        let verified = parse_manifest(&input.document_bytes)?;
        let import_trust = trust_tier_for_source_ref(&input.source_metadata.source_ref);
        let (source, document, entry) = self
            .build_single_governed_import(&verified, &input.document_bytes, input.source_metadata)
            .await?;
        self.repository
            .save_governed_catalog_import(SaveGovernedCatalogImport {
                source: source.clone(),
                document: document.clone(),
                verified_source_document: None,
                entries: vec![entry],
            })
            .await?;
        let stored = self
            .repository
            .get_governed_catalog_entry(
                &source.source_id,
                &verified.manifest().id,
                verified.manifest().version.as_str(),
            )
            .await?;
        Ok(GovernedImportResult {
            source: GovernedCatalogSourceSummary {
                source_id: source.source_id,
                import_kind: source.import_kind,
                display_name: source.display_name,
            },
            document: GovernedCatalogDocumentSummary {
                source_id: document.source_id,
                document_id: document.document_id,
                document_digest: document.document_digest,
                document_kind: document.document_kind,
            },
            entries: vec![catalog_summary_from_entry(
                stored,
                self.options.compatibility_target,
                self.eligibility_for_manifest(&verified, import_trust),
            )],
        })
    }

    pub async fn governed_directory_import(
        &self,
        _context: &RequestContext,
        input: GovernedDirectoryImportInput,
    ) -> McpPlatformResult<GovernedImportResult> {
        self.require_mutation_authority()?;
        validate_verified_catalog_directory_metadata(&input.source_metadata)?;
        let verified_document =
            crate::verified_source_catalog::parse_signed_envelope(&input.directory_document_bytes)
                .map_err(|_| invalid_request())?;
        validate_verified_directory_provenance(&input.source_metadata, &verified_document)?;
        let now_ms = self.clock.now_ms();
        let source = governed_source_record(
            &input.source_metadata.source_ref,
            Some(verified_document.envelope.payload.source_name.as_str()),
            now_ms,
        )?;
        let document = governed_directory_document_record(
            &source.source_id,
            &verified_document.digests.document_digest,
            now_ms,
        );
        let mut saved_entries = Vec::new();
        for manifest in input.manifests {
            let verified = parse_manifest(&manifest.document_bytes)?;
            if !verified_source_document_contains_release(
                &verified_document,
                &manifest.release_id,
                &verified.manifest().id,
                verified.manifest().version.as_str(),
            ) {
                return Err(invalid_request());
            }
            let source_metadata = ManifestSourceMetadata {
                source_ref: input.source_metadata.source_ref.clone(),
                import_kind: input.source_metadata.import_kind,
                release_id: Some(manifest.release_id.clone()),
                origin_provenance: input.source_metadata.origin_provenance.clone(),
                update_channel: input.source_metadata.update_channel.clone(),
            };
            let trust_tier = trust_tier_for_source_ref(&source_metadata.source_ref);
            ensure_import_eligible(self.eligibility_for_manifest(&verified, trust_tier))?;
            saved_entries.push(build_governed_catalog_entry(
                &verified,
                source_metadata,
                now_ms,
                Some(manifest.release_id),
            )?);
        }
        self.repository
            .save_governed_catalog_import(SaveGovernedCatalogImport {
                source: source.clone(),
                document: document.clone(),
                verified_source_document: Some(verified_document),
                entries: saved_entries,
            })
            .await?;
        let all_entries = self.repository.list_governed_catalog_entries().await?;
        let items = all_entries
            .into_iter()
            .filter(|entry| {
                entry.source.source_id == source.source_id
                    && entry.document.document_id == document.document_id
            })
            .map(|entry| {
                let eligibility = self
                    .eligibility_for_manifest(&entry.manifest.verified, entry.manifest.trust_tier);
                catalog_summary_from_entry(entry, self.options.compatibility_target, eligibility)
            })
            .collect();
        Ok(GovernedImportResult {
            source: GovernedCatalogSourceSummary {
                source_id: source.source_id,
                import_kind: source.import_kind,
                display_name: source.display_name,
            },
            document: GovernedCatalogDocumentSummary {
                source_id: document.source_id,
                document_id: document.document_id,
                document_digest: document.document_digest,
                document_kind: document.document_kind,
            },
            entries: items,
        })
    }

    pub async fn governed_source_refresh(
        &self,
        context: &RequestContext,
        input: GovernedSourceRefreshInput,
    ) -> McpPlatformResult<GovernedSourceRefreshResult> {
        validate_identifier(&input.source_id)?;
        let now_ms = self.clock.now_ms();
        let result = self
            .governed_source_refresh_inner(context, &input.source_id, now_ms)
            .await;
        if let Err(error) = &result {
            self.repository
                .record_governed_source_refresh_audit(RecordGovernedSourceRefreshAudit {
                    source_id: &input.source_id,
                    document_digest: None,
                    result: GovernedSourceRefreshResultState::Failed,
                    error_code: Some(error.code().as_str()),
                    actor: context.actor(),
                    correlation_id: context.correlation_id(),
                    occurred_at_ms: now_ms,
                })
                .await?;
        }
        result
    }

    pub(crate) async fn source_provision_prepare(
        &self,
        context: &RequestContext,
        input: SourceProvisionPrepareInput,
    ) -> McpPlatformResult<SourceProvisionPrepareResult> {
        let transport_session_binding = context
            .transport_session_binding()
            .filter(|binding| !binding.is_empty())
            .ok_or_else(source_provisioning_transport_required)?;
        let parsed = parse_source_provisioning_snapshot(&input.frozen)?;
        let provision_id = self.ids.next_id("source_provision");
        let confirmation_token = format!(
            "source_provisioning_confirmation_{}",
            self.ids.next_id("token")
        );
        let (stage, confirmation) = self.source_provisioning.stage(
            provision_id,
            confirmation_token,
            context.actor(),
            transport_session_binding,
            input.frozen,
            &parsed,
            self.clock.now_ms(),
        )?;
        Ok(SourceProvisionPrepareResult {
            provision_id: confirmation.provision_id,
            confirmation_token: confirmation.confirmation_token,
            expires_at_ms: confirmation.expires_at_ms,
            preview: source_provision_preview_from_stage(&stage.preview),
        })
    }

    pub(crate) async fn source_provision_confirm(
        &self,
        context: &RequestContext,
        input: SourceProvisionConfirmInput,
    ) -> McpPlatformResult<SourceProvisionConfirmResult> {
        let transport_session_binding = context
            .transport_session_binding()
            .filter(|binding| !binding.is_empty())
            .ok_or_else(source_provisioning_transport_required)?;
        let confirmation_hash = sha256_hex(input.confirmation_token.as_bytes());
        let stage = self.source_provisioning.claim(
            &input.provision_id,
            &confirmation_hash,
            context.actor(),
            transport_session_binding,
            self.clock.now_ms(),
        )?;
        if !input.confirm {
            self.source_provisioning
                .finish(&input.provision_id, &confirmation_hash, false);
            return Ok(SourceProvisionConfirmResult {
                provision_id: stage.provision_id,
                confirmed_at_ms: self.clock.now_ms(),
                preview: source_provision_preview_from_stage(&stage.preview),
            });
        }

        let occurred_at_ms = self.clock.now_ms();
        let result = async {
            let parsed = parse_source_provisioning_snapshot(&stage.frozen)?;
            let import =
                self.build_source_provision_import(&stage.frozen, &parsed, occurred_at_ms)?;
            self.repository
                .confirm_source_provisioning(ConfirmSourceProvisioning {
                    provision_id: stage.provision_id.clone(),
                    snapshot_digest: parsed.snapshot_digest,
                    frozen: stage.frozen.clone(),
                    import,
                    actor: context.actor().to_string(),
                    correlation_id: stage.provision_id.clone(),
                    occurred_at_ms,
                })
                .await?;
            Ok::<_, McpPlatformError>(())
        }
        .await;
        self.source_provisioning
            .finish(&input.provision_id, &confirmation_hash, result.is_ok());
        result?;
        Ok(SourceProvisionConfirmResult {
            provision_id: stage.provision_id,
            confirmed_at_ms: self.clock.now_ms(),
            preview: source_provision_preview_from_stage(&stage.preview),
        })
    }

    fn build_source_provision_import(
        &self,
        frozen: &FrozenSourceProvisioningSnapshot,
        parsed: &crate::mcp_platform::source_provisioning::ParsedSourceProvisioningSnapshot,
        now_ms: i64,
    ) -> McpPlatformResult<SaveGovernedCatalogImport> {
        let directory_import =
            build_governed_source_refresh_input(frozen, parsed, &parsed.descriptor.source_id)?;
        let source = governed_source_record(
            &directory_import.source_metadata.source_ref,
            Some(parsed.source_document.envelope.payload.source_name.as_str()),
            now_ms,
        )?;
        let document = governed_directory_document_record(
            &source.source_id,
            &parsed.source_document.digests.document_digest,
            now_ms,
        );
        let entries = frozen
            .manifests
            .iter()
            .map(|manifest| {
                let verified = parse_manifest(&manifest.document_bytes)?;
                let trust_tier =
                    trust_tier_for_source_ref(&directory_import.source_metadata.source_ref);
                ensure_import_eligible(self.eligibility_for_manifest(&verified, trust_tier))?;
                build_governed_catalog_entry(
                    &verified,
                    ManifestSourceMetadata {
                        source_ref: directory_import.source_metadata.source_ref.clone(),
                        import_kind: directory_import.source_metadata.import_kind,
                        release_id: Some(manifest.release_id.clone()),
                        origin_provenance: directory_import
                            .source_metadata
                            .origin_provenance
                            .clone(),
                        update_channel: directory_import.source_metadata.update_channel.clone(),
                    },
                    now_ms,
                    Some(manifest.release_id.clone()),
                )
            })
            .collect::<McpPlatformResult<Vec<_>>>()?;
        Ok(SaveGovernedCatalogImport {
            source,
            document,
            verified_source_document: Some(parsed.source_document.clone()),
            entries,
        })
    }

    async fn governed_source_refresh_inner(
        &self,
        context: &RequestContext,
        source_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<GovernedSourceRefreshResult> {
        let source = self
            .repository
            .get_governed_catalog_source(source_id)
            .await?;
        if !matches!(source.source_ref, SourceRef::VerifiedSourceCatalog { source_id: ref id } if id == source_id)
            || source.import_kind != SourceImportKind::VerifiedSourceCatalog
        {
            return Err(invalid_request());
        }
        let registration = self
            .repository
            .get_governed_source_refresh_registration(source_id)
            .await
            .map_err(|error| {
                if error.code() == McpPlatformErrorCode::NotFound {
                    refresh_not_registered()
                } else {
                    error
                }
            })?;
        let trust_anchor = self
            .repository
            .get_governed_source_refresh_trust_anchor(source_id)
            .await?;
        let frozen = self
            .governed_source_refresh_fetcher
            .fetch(&registration.endpoint)
            .await?;
        let parsed = parse_source_provisioning_snapshot(&frozen)?;
        if parsed.descriptor.source_id != source_id
            || parsed.descriptor.endpoint != registration.endpoint
            || parsed.descriptor.root_digest != trust_anchor.anchor.root_digest
        {
            return Err(invalid_request());
        }
        let import = self.build_source_provision_import(&frozen, &parsed, now_ms)?;
        let document_digest = import.document.document_digest.clone();
        let display_name = import.source.display_name.clone();
        let manifest_count = import
            .entries
            .len()
            .try_into()
            .map_err(|_| invalid_request())?;
        self.repository
            .apply_governed_source_refresh(ApplyGovernedSourceRefresh {
                source_id: source_id.to_string(),
                expected_endpoint: registration.endpoint.clone(),
                expected_anchor_root_digest: trust_anchor.anchor.root_digest.clone(),
                import,
                actor: context.actor().to_string(),
                correlation_id: context.correlation_id().to_string(),
                occurred_at_ms: now_ms,
            })
            .await?;
        Ok(GovernedSourceRefreshResult {
            source_id: source_id.to_string(),
            display_name,
            document_digest,
            manifest_count,
            refreshed_at_ms: now_ms,
        })
    }

    async fn build_single_governed_import(
        &self,
        verified: &VerifiedManifest,
        document_bytes: &[u8],
        source_metadata: ManifestSourceMetadata,
    ) -> McpPlatformResult<(
        GovernedCatalogSourceRecord,
        GovernedCatalogDocumentRecord,
        SaveGovernedCatalogEntry,
    )> {
        validate_supported_governed_source_metadata(&source_metadata)?;
        if matches!(
            source_metadata.source_ref,
            SourceRef::VerifiedSourceCatalog { .. }
        ) && source_metadata.release_id.is_none()
        {
            return Err(invalid_request());
        }
        if let SourceRef::VerifiedSourceCatalog { source_id } = &source_metadata.source_ref {
            let stored = self
                .repository
                .source_catalog_document_provenance(source_id)
                .await?;
            validate_verified_provenance_against_stored(&source_metadata, &stored)?;
            if !self
                .repository
                .source_catalog_release_matches(
                    source_id,
                    source_metadata
                        .release_id
                        .as_deref()
                        .ok_or_else(invalid_request)?,
                    &verified.manifest().id,
                    verified.manifest().version.as_str(),
                )
                .await?
            {
                return Err(invalid_request());
            }
        }
        let trust_tier = trust_tier_for_source_ref(&source_metadata.source_ref);
        ensure_import_eligible(self.eligibility_for_manifest(verified, trust_tier))?;
        let now_ms = self.clock.now_ms();
        let display_name = match &source_metadata.source_ref {
            SourceRef::VerifiedSourceCatalog { source_id } => Some(
                self.repository
                    .source_catalog_document_provenance(source_id)
                    .await?
                    .source_name,
            ),
            _ => None,
        };
        let source =
            governed_source_record(&source_metadata.source_ref, display_name.as_deref(), now_ms)?;
        let document = governed_manifest_document_record(&source.source_id, document_bytes, now_ms);
        let entry = build_governed_catalog_entry(verified, source_metadata, now_ms, None)?;
        Ok((source, document, entry))
    }
    pub fn production(repository: Arc<SqliteMcpPlatformRepository>) -> Self {
        let remote_http = Arc::new(CoreManagedRemoteHttpNetworkPolicy::default());
        Self::new_internal(
            repository.clone(),
            Arc::new(SystemClock),
            Arc::new(UuidGenerator),
            McpPlatformServiceOptions::default(),
            lifecycle_ports(remote_http.clone()),
            true,
            remote_http,
        )
    }

    pub fn new(
        repository: Arc<SqliteMcpPlatformRepository>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        options: McpPlatformServiceOptions,
    ) -> Self {
        let remote_http = Arc::new(UnavailableRemoteHttpNetworkPolicy);
        Self::new_internal(
            repository.clone(),
            clock,
            ids,
            options,
            lifecycle_ports(remote_http.clone()),
            false,
            remote_http,
        )
    }

    #[cfg(test)]
    pub(crate) fn new_trusted(
        repository: Arc<SqliteMcpPlatformRepository>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        options: McpPlatformServiceOptions,
    ) -> Self {
        Self::new(repository, clock, ids, options)
    }

    pub fn new_with_lifecycle_ports(
        repository: Arc<SqliteMcpPlatformRepository>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        options: McpPlatformServiceOptions,
        ports: LifecyclePorts,
        remote_http_network_policy: Arc<dyn RemoteHttpNetworkPolicy>,
    ) -> Self {
        Self::new_internal(
            repository.clone(),
            clock,
            ids,
            options,
            ports,
            false,
            remote_http_network_policy,
        )
    }

    pub fn new_with_remote_http_network_policy(
        repository: Arc<SqliteMcpPlatformRepository>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        options: McpPlatformServiceOptions,
        remote_http: Arc<dyn RemoteHttpNetworkPolicy>,
    ) -> Self {
        Self::new_internal(
            repository.clone(),
            clock,
            ids,
            options,
            lifecycle_ports(remote_http.clone()),
            false,
            remote_http,
        )
    }

    pub fn new_with_distribution_ports(
        repository: Arc<SqliteMcpPlatformRepository>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        options: McpPlatformServiceOptions,
        ports: LifecyclePorts,
        distribution: Arc<dyn DistributionEffectAdapter>,
        runtime_capabilities: RuntimeCapabilitySnapshot,
    ) -> Self {
        let development_mode = options.development_mode;
        let runner = Arc::new(
            TaskRunner::new(
                repository.clone(),
                clock.clone(),
                ports,
                ids.next_id("worker"),
            )
            .with_distribution_adapter(distribution),
        );
        Self {
            repository: repository.clone(),
            clock,
            ids,
            options,
            runner,
            auto_worker: false,
            worker: Mutex::new(WorkerState::NotStarted),
            runtime_capabilities,
            external_capabilities: ExternalCapabilitySnapshot::default(),
            development_mode,
            manual_stdio_provider: Arc::new(UnsupportedManualStdioProvider),
            remote_http_network_policy: Arc::new(UnavailableRemoteHttpNetworkPolicy),
            governed_source_refresh_fetcher: Arc::new(RemoteGovernedSourceRefreshFetcher::new(
                Arc::new(UnavailableRemoteHttpNetworkPolicy),
            )),
            https_manifest_fetcher: Arc::new(UnavailableHttpsManifestSourceFetcher),
            #[cfg(feature = "integration-test-support")]
            https_manifest_confirm_stages: Mutex::new(HashMap::new()),
            source_provisioning: SourceProvisioningCoordinator::default(),
            #[cfg(test)]
            credential_enrollment: EnrollmentCoordinator::default(),
            #[cfg(test)]
            credential_writer_owner: EnrollmentWriterOwner::in_memory_for_testing_default_time(),
            #[cfg(test)]
            remote_inspection_repository: repository.clone(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_external_capabilities(
        repository: Arc<SqliteMcpPlatformRepository>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        options: McpPlatformServiceOptions,
        ports: LifecyclePorts,
        distribution: Arc<dyn DistributionEffectAdapter>,
        runtime_capabilities: RuntimeCapabilitySnapshot,
        docker_available: bool,
        git_available: bool,
    ) -> Self {
        let docker_policy_allowed = options.docker_daemon_policy_allowed;
        let development_mode = options.development_mode;
        let runner = Arc::new(
            TaskRunner::new(
                repository.clone(),
                clock.clone(),
                ports,
                ids.next_id("worker"),
            )
            .with_distribution_adapter(distribution),
        );
        Self {
            repository: repository.clone(),
            clock,
            ids,
            options,
            runner,
            auto_worker: false,
            worker: Mutex::new(WorkerState::NotStarted),
            runtime_capabilities,
            external_capabilities: ExternalCapabilitySnapshot {
                docker_available,
                docker_daemon_verified: false,
                docker_policy_allowed,
                git_available,
            },
            development_mode,
            manual_stdio_provider: Arc::new(UnsupportedManualStdioProvider),
            remote_http_network_policy: Arc::new(UnavailableRemoteHttpNetworkPolicy),
            governed_source_refresh_fetcher: Arc::new(RemoteGovernedSourceRefreshFetcher::new(
                Arc::new(UnavailableRemoteHttpNetworkPolicy),
            )),
            https_manifest_fetcher: Arc::new(UnavailableHttpsManifestSourceFetcher),
            #[cfg(feature = "integration-test-support")]
            https_manifest_confirm_stages: Mutex::new(HashMap::new()),
            source_provisioning: SourceProvisioningCoordinator::default(),
            #[cfg(test)]
            credential_enrollment: EnrollmentCoordinator::default(),
            #[cfg(test)]
            credential_writer_owner: EnrollmentWriterOwner::in_memory_for_testing_default_time(),
            #[cfg(test)]
            remote_inspection_repository: repository.clone(),
        }
    }

    fn new_internal(
        repository: Arc<SqliteMcpPlatformRepository>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        options: McpPlatformServiceOptions,
        ports: LifecyclePorts,
        auto_worker: bool,
        remote_http_network_policy: Arc<dyn RemoteHttpNetworkPolicy>,
    ) -> Self {
        let development_mode = options.development_mode;
        let runtime = RuntimeCapabilities::discover();
        let runtime_capabilities = runtime.snapshot();
        let external = ExternalDistributionPorts::production_with_docker_policy(
            options.docker_daemon_policy_allowed,
        );
        let external_capabilities = external.snapshot();
        let mut runner = TaskRunner::new(
            repository.clone(),
            clock.clone(),
            ports,
            ids.next_id("worker"),
        );
        if let Ok(adapter) = ProductionDistributionEffectAdapter::new_with_runtime_and_external(
            crate::config::paths::Paths::in_data_dir("mcp-platform"),
            runtime,
            external,
        ) {
            runner = runner.with_distribution_adapter(Arc::new(adapter));
        }
        let runner = Arc::new(runner);
        Self {
            repository: repository.clone(),
            clock,
            ids,
            options,
            runner,
            auto_worker,
            worker: Mutex::new(WorkerState::NotStarted),
            runtime_capabilities,
            external_capabilities,
            development_mode,
            manual_stdio_provider: Arc::new(UnsupportedManualStdioProvider),
            remote_http_network_policy: remote_http_network_policy.clone(),
            governed_source_refresh_fetcher: Arc::new(RemoteGovernedSourceRefreshFetcher::new(
                remote_http_network_policy.clone(),
            )),
            https_manifest_fetcher: Arc::new(ProductionHttpsManifestSourceFetcher::new()),
            #[cfg(feature = "integration-test-support")]
            https_manifest_confirm_stages: Mutex::new(HashMap::new()),
            source_provisioning: SourceProvisioningCoordinator::default(),
            #[cfg(test)]
            credential_enrollment: EnrollmentCoordinator::default(),
            #[cfg(test)]
            credential_writer_owner: EnrollmentWriterOwner::in_memory_for_testing_default_time(),
            #[cfg(test)]
            remote_inspection_repository: repository.clone(),
        }
    }

    pub fn with_manual_stdio_provider(mut self, provider: Arc<dyn ManualStdioProvider>) -> Self {
        self.manual_stdio_provider = provider;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_enrollment_writer_owner(mut self, owner: EnrollmentWriterOwner) -> Self {
        self.credential_writer_owner = owner;
        self
    }

    pub fn with_governed_source_refresh_fetcher(
        mut self,
        fetcher: Arc<dyn GovernedSourceRefreshFetcher>,
    ) -> Self {
        self.governed_source_refresh_fetcher = fetcher;
        self
    }

    #[cfg(any(test, feature = "integration-test-support"))]
    pub fn with_https_manifest_fetcher(
        mut self,
        fetcher: Arc<dyn HttpsManifestSourceFetcher>,
    ) -> Self {
        self.https_manifest_fetcher = fetcher;
        self
    }

    fn record_https_manifest_confirm_stage(
        &self,
        provision_id: &str,
        stage: HttpsManifestConfirmStage,
    ) {
        #[cfg(feature = "integration-test-support")]
        self.https_manifest_confirm_stages
            .lock()
            .expect("HTTPS manifest confirm stage lock")
            .insert(provision_id.to_owned(), stage);
        #[cfg(not(feature = "integration-test-support"))]
        let _ = (provision_id, stage);
    }

    #[cfg(feature = "integration-test-support")]
    pub fn https_manifest_confirm_stage(&self, provision_id: &str) -> Option<&'static str> {
        self.https_manifest_confirm_stages
            .lock()
            .expect("HTTPS manifest confirm stage lock")
            .get(provision_id)
            .copied()
            .map(HttpsManifestConfirmStage::as_str)
    }

    pub fn start_worker_if_configured(&self) {
        if !self.auto_worker {
            return;
        }
        let mut state = self.worker.lock().expect("MCP worker state lock");
        if matches!(*state, WorkerState::NotStarted) {
            *state = WorkerState::Running(tokio::spawn(self.runner.clone().run()));
        }
    }

    #[cfg(test)]
    pub(crate) fn test_worker_started(&self) -> bool {
        matches!(
            &*self.worker.lock().expect("MCP worker state lock"),
            WorkerState::Running(_)
        )
    }

    pub async fn runner_tick(&self) -> McpPlatformResult<bool> {
        self.runner.tick().await
    }

    pub async fn shutdown_worker(&self) {
        let handle = {
            let mut state = self.worker.lock().expect("MCP worker state lock");
            match std::mem::replace(&mut *state, WorkerState::Stopped) {
                WorkerState::Running(handle) => Some(handle),
                WorkerState::NotStarted | WorkerState::Stopped => None,
            }
        };
        self.runner.shutdown();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }

    pub fn trusted_local_context(&self) -> RequestContext {
        RequestContext::local_authenticated_client(self.ids.next_id("correlation"))
    }

    #[cfg(test)]
    pub(crate) fn new_enrollment_user_action_binding(&self) -> UserActionBinding {
        UserActionBinding::new(self.ids.next_id("enrollment_action"))
    }

    #[cfg(test)]
    pub(crate) async fn credential_enrollment_begin(
        &self,
        _context: &RequestContext,
        input: ManagedCredentialEnrollmentBeginInput,
    ) -> McpPlatformResult<ManagedCredentialEnrollmentBeginView> {
        let inventory = self
            .repository
            .get_managed_inventory(&input.managed_mcp_id)
            .await?;
        if inventory.managed.revision != input.expected_revision {
            return Err(revision_conflict());
        }
        let manifest_digest = inventory
            .lifecycle
            .active_manifest_digest
            .ok_or_else(integrity_error)?;
        let manifest = self.repository.get_manifest(&manifest_digest).await?;
        let schema = trusted_credential_enrollment_schema(&manifest.verified.manifest().auth)
            .map_err(|_| operation_not_supported())?;
        let view = self.credential_enrollment.begin(
            BeginEnrollmentSessionRequest {
                user_action_binding: input.user_action_binding,
                managed_mcp_id: input.managed_mcp_id,
                manifest_digest,
                schema: crate::mcp_platform::credential_enrollment::CredentialEnrollmentSchemaView::from_manifest(
                    &schema,
                ),
                expected_revision: input.expected_revision,
                profile_scope: input.profile_scope,
                authority_instance_id: self.credential_writer_owner.instance_id().to_string(),
            },
            self.clock.now_ms(),
        )?;
        Ok(ManagedCredentialEnrollmentBeginView {
            session_token: view.session_token,
            managed_mcp_id: view.managed_mcp_id,
            manifest_digest: view.manifest_digest,
            schema: view.schema,
            expected_revision: view.expected_revision,
            profile_scope: view.profile_scope,
            expires_at_ms: view.expires_at_ms,
        })
    }

    #[cfg(test)]
    pub(crate) fn set_enrollment_runtime_binding_resolver(
        &self,
        resolver: Arc<dyn EnrollmentRuntimeBindingResolver>,
    ) {
        self.runner
            .set_enrollment_runtime_binding_resolver(resolver);
    }

    #[cfg(test)]
    pub(crate) async fn get_managed_credential_enrollment(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<crate::mcp_platform::repository::ManagedCredentialEnrollmentRecord>>
    {
        self.repository
            .get_managed_credential_enrollment(managed_mcp_id)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn projection_recovery_required(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<bool> {
        self.repository
            .projection_recovery_required(managed_mcp_id)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn resolve_projection_recovery(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<()> {
        self.runner
            .resolve_projection_recovery(managed_mcp_id)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn intake_submit_manual_https_candidate(
        &self,
        _context: &RequestContext,
        input: ManualHttpsCandidateInput,
        submission_binding: &str,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        let (display_origin, private_endpoint_ref) = match input {
            ManualHttpsCandidateInput::Candidate {
                display_origin,
                private_endpoint_ref,
            } => (display_origin, private_endpoint_ref),
            ManualHttpsCandidateInput::RemoteHttpPolicyUnavailable => {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::RemoteHttpPolicyUnavailable,
                    "remote HTTP policy is unavailable",
                ));
            }
            ManualHttpsCandidateInput::UnsupportedTransport => {
                return Err(operation_not_supported());
            }
        };
        validate_intake_raw_reference(&private_endpoint_ref)?;
        let descriptor = IntakeConfigurationDescriptor::ManualHttpsCandidate { display_origin };
        self.save_intake_candidate(
            descriptor,
            IntakeSourceFacet::ManualHttpsCandidate,
            IntakeTransport::StreamableHttp,
            IntakeLifecycleState::AwaitingConsent,
            Some(IntakePrivateReferenceKind::ManualHttpsEndpoint),
            submission_binding,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn intake_submit_approved_stdio_candidate(
        &self,
        _context: &RequestContext,
        input: ApprovedStdioCandidateInput,
        submission_binding: &str,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        let (provider_source_ref, private_payload_ref, declared_mcp_id, declared_version) =
            match input {
                ApprovedStdioCandidateInput::Candidate {
                    provider_source_ref,
                    private_payload_ref,
                    declared_mcp_id,
                    declared_version,
                } => (
                    provider_source_ref,
                    private_payload_ref,
                    declared_mcp_id,
                    declared_version,
                ),
                ApprovedStdioCandidateInput::ManualStdioProviderUnavailable => {
                    return Err(McpPlatformError::new(
                        McpPlatformErrorCode::ManualStdioProviderUnavailable,
                        "manual stdio provider is unavailable",
                    ));
                }
                ApprovedStdioCandidateInput::EmptyProvider => return Err(operation_not_supported()),
                ApprovedStdioCandidateInput::UnsupportedTransport => {
                    return Err(operation_not_supported());
                }
            };
        validate_intake_raw_reference(&private_payload_ref)?;
        let descriptor = IntakeConfigurationDescriptor::ApprovedStdioCandidate {
            provider_source_ref,
            declared_mcp_id,
            declared_version,
        };
        self.save_intake_candidate(
            descriptor,
            IntakeSourceFacet::ApprovedStdioCandidate,
            IntakeTransport::Stdio,
            IntakeLifecycleState::Submitted,
            Some(IntakePrivateReferenceKind::ApprovedStdioPayload),
            submission_binding,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn intake_submit_catalog_planning_candidate(
        &self,
        _context: &RequestContext,
        input: CatalogPlanningCandidateInput,
        submission_binding: &str,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        if let Some(reference) = &input.private_manifest_ref {
            validate_intake_raw_reference(reference)?;
        }
        let descriptor = IntakeConfigurationDescriptor::CatalogPlanning {
            source_id: input.source_id,
            mcp_id: input.mcp_id,
            version: input.version,
        };
        self.save_intake_candidate(
            descriptor,
            IntakeSourceFacet::CatalogPlanning,
            IntakeTransport::CatalogReference,
            IntakeLifecycleState::Submitted,
            Some(IntakePrivateReferenceKind::CatalogManifest),
            submission_binding,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn intake_submit_legacy_quarantine(
        &self,
        _context: &RequestContext,
        input: LegacyQuarantineCandidateInput,
        submission_binding: &str,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        self.save_intake_candidate(
            IntakeConfigurationDescriptor::LegacyQuarantine {
                legacy_flow: input.legacy_flow,
            },
            IntakeSourceFacet::LegacyQuarantine,
            IntakeTransport::Legacy,
            IntakeLifecycleState::LegacyQuarantine,
            None,
            submission_binding,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn intake_get_candidate(
        &self,
        candidate_id: &str,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        self.repository.get_intake_candidate(candidate_id).await
    }

    #[cfg(test)]
    pub(crate) async fn intake_grant_consent(
        &self,
        candidate_id: &str,
        consent_binding: &str,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        self.repository
            .record_intake_consent_grant(crate::mcp_platform::intake::RecordIntakeConsentGrant {
                candidate_id,
                consent_binding,
                now_ms: self.clock.now_ms(),
            })
            .await
    }

    #[cfg(test)]
    pub(crate) async fn intake_grant_approval(
        &self,
        candidate_id: &str,
        approval_binding: &str,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        self.repository
            .record_intake_approval_grant(crate::mcp_platform::intake::RecordIntakeApprovalGrant {
                candidate_id,
                approval_binding,
                now_ms: self.clock.now_ms(),
            })
            .await
    }

    #[cfg(test)]
    pub(crate) async fn intake_record_binding_ready(
        &self,
        candidate_id: &str,
        manifest_identity_binding: &str,
        registry_owned_payload_digest: Option<&str>,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        self.repository
            .record_intake_binding_ready(crate::mcp_platform::intake::RecordIntakeBindingReady {
                candidate_id,
                manifest_identity_binding,
                registry_owned_payload_digest,
                now_ms: self.clock.now_ms(),
            })
            .await
    }

    #[cfg(test)]
    pub(crate) async fn intake_grant_batch1_inspection_consent(
        &self,
        candidate_id: &str,
        consent_seed: &str,
        expires_at_ms: i64,
    ) -> McpPlatformResult<crate::mcp_platform::intake_inspection::InspectionConsentRecord> {
        let candidate = self.intake_get_candidate(candidate_id).await?;
        let binding = batch1_consent_tuple(&candidate, sha256_hex(consent_seed.as_bytes()))?;
        self.repository
            .save_intake_inspection_consent(SaveIntakeInspectionConsent {
                consent_id: self.ids.next_id("inspection_consent"),
                candidate_id: candidate.candidate_id.clone(),
                candidate_lifecycle_state: candidate.lifecycle_state,
                binding,
                expires_at_ms,
                created_at_ms: self.clock.now_ms(),
            })
            .await
    }

    #[cfg(test)]
    pub(crate) async fn intake_request_batch1_inspection_snapshot(
        &self,
        candidate_id: &str,
        consent_id: &str,
    ) -> McpPlatformResult<crate::mcp_platform::intake_inspection::StoredInspectionSnapshot> {
        let candidate = self.intake_get_candidate(candidate_id).await?;
        let consent = self
            .repository
            .get_intake_inspection_consent(consent_id)
            .await?;
        let snapshot_id = self.ids.next_id("inspection_snapshot");
        let snapshot = match InspectionPurpose::derive(candidate.source_facet, candidate.transport)?
        {
            InspectionPurpose::RemoteCandidateBoundary => build_remote_blocked_snapshot(
                snapshot_id,
                candidate.candidate_id.clone(),
                consent.consent_id.clone(),
                consent.binding.clone(),
                self.clock.now_ms(),
            ),
            InspectionPurpose::ApprovedStdioLocalMetadata => build_stdio_local_snapshot(
                snapshot_id,
                candidate.candidate_id.clone(),
                consent.consent_id.clone(),
                consent.binding.clone(),
                self.clock.now_ms(),
            ),
        };
        self.repository
            .record_intake_inspection_snapshot(RecordIntakeInspectionSnapshot {
                snapshot,
                candidate_lifecycle_state: candidate.lifecycle_state,
            })
            .await
    }

    #[cfg(test)]
    pub(crate) async fn get_intake_inspection_snapshot(
        &self,
        snapshot_id: &str,
    ) -> McpPlatformResult<crate::mcp_platform::intake_inspection::StoredInspectionSnapshot> {
        self.repository
            .get_intake_inspection_snapshot(snapshot_id)
            .await
    }

    #[cfg(test)]
    async fn save_intake_candidate(
        &self,
        descriptor: IntakeConfigurationDescriptor,
        source_facet: IntakeSourceFacet,
        transport: IntakeTransport,
        initial_state: IntakeLifecycleState,
        private_kind: Option<IntakePrivateReferenceKind>,
        submission_binding: &str,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        let encoded = serde_json::to_vec(&descriptor).map_err(|_| invalid_request())?;
        let configuration_ref = self.ids.next_id("intake_config");
        let private_reference = private_kind
            .map(|kind| self.repository.mint_intake_private_reference(kind))
            .transpose()?;
        let mut digest_input = encoded.clone();
        digest_input.push(0);
        if let Some(reference) = &private_reference {
            digest_input.extend_from_slice(reference.as_bytes());
        }
        let descriptor_digest = sha256_hex(&digest_input);
        let configuration_ref = self
            .repository
            .save_intake_configuration_ref(SaveIntakeConfigurationRef {
                configuration_ref: &configuration_ref,
                descriptor: &descriptor,
                private_reference: private_reference.as_deref(),
                descriptor_digest: &descriptor_digest,
                now_ms: self.clock.now_ms(),
            })
            .await?;
        let candidate_id = self.ids.next_id("intake_candidate");
        self.repository
            .save_intake_candidate(SaveIntakeCandidate {
                candidate_id: &candidate_id,
                submission_binding,
                source_facet,
                transport,
                initial_state,
                redacted_configuration_ref: &configuration_ref,
                descriptor_digest: &descriptor_digest,
                now_ms: self.clock.now_ms(),
            })
            .await
    }

    #[cfg(test)]
    pub(crate) async fn credential_enrollment_submit(
        &self,
        _context: &RequestContext,
        input: ManagedCredentialEnrollmentSubmitInput,
    ) -> McpPlatformResult<ManagedCredentialEnrollmentSubmitView> {
        let session = self
            .credential_enrollment
            .peek(&input.session_token)
            .ok_or_else(invalid_request)?;
        let consumed = self.credential_enrollment.consume(
            ConsumeEnrollmentSessionRequest {
                session_token: input.session_token,
                user_action_binding: input.user_action_binding,
                managed_mcp_id: session.managed_mcp_id.clone(),
                manifest_digest: session.manifest_digest.clone(),
                schema_id: session.schema.schema_id.clone(),
                expected_revision: session.expected_revision,
                profile_scope: input.current_profile_scope,
                authority_instance_id: self.credential_writer_owner.instance_id().to_string(),
            },
            self.clock.now_ms(),
        )?;
        let submission = self
            .credential_enrollment
            .validate_submission(&consumed.schema, input.fields)?;
        let handle = CredentialHandle::parse(format!("enr.{}", consumed.managed_mcp_id))
            .map_err(|_| invalid_request())?;
        let binding = CredentialReadinessBinding::auth_requirement_probe(
            "managed_enrollment_runtime",
            &consumed.managed_mcp_id,
        );
        let written = self
            .credential_writer_owner
            .write_validated_secret(&handle, submission.secret_bytes(), &binding)
            .map_err(|_| credential_missing())?;
        let authority = self.credential_writer_owner.authority_summary();
        let authority_record = ManagedCredentialEnrollmentAuthoritySummary {
            provider_id: authority.provider_id,
            writer_mode: authority.writer_mode,
        };
        let reference_digest = credential_reference_digest(&written.handle);
        let record = self
            .repository
            .save_managed_credential_enrollment(SaveManagedCredentialEnrollment {
                managed_mcp_id: &consumed.managed_mcp_id,
                manifest_digest: &consumed.manifest_digest,
                auth_schema_id: &consumed.schema.schema_id,
                credential_reference: &written.handle,
                reference_digest: &reference_digest,
                authority: &authority_record,
                authority_evidence_digest: &written.evidence_digest,
                now_ms: self.clock.now_ms(),
            })
            .await?;
        Ok(ManagedCredentialEnrollmentSubmitView {
            managed_mcp_id: record.managed_mcp_id,
            manifest_digest: record.manifest_digest,
            credential_status: CredentialStatus::Ready,
            enrollment_revision: record.revision,
            updated_at_ms: record.updated_at_ms,
            redacted_field_count: submission.redacted_field_count,
        })
    }

    #[cfg(test)]
    pub(crate) async fn gate_validated_remote_inspection_reserve_v26(
        &self,
        input: ReserveOrLoadRemoteInspection<'_>,
    ) -> McpPlatformResult<
        crate::mcp_platform::intake_remote_inspection::RemoteInspectionReservationRecord,
    > {
        if matches!(
            input.source_facet,
            crate::mcp_platform::intake::IntakeSourceFacet::CatalogPlanning
        ) {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::OperationNotSupported,
                "remote_inspection_transport_unsupported",
            ));
        }
        self.remote_inspection_repository
            .reserve_or_load_active_remote_inspection(input)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn gate_validated_remote_inspection_tombstone_v26(
        &self,
        input: TombstoneRemoteInspectionPrebind<'_>,
    ) -> McpPlatformResult<
        crate::mcp_platform::intake_remote_inspection::RemoteInspectionReservationRecord,
    > {
        self.remote_inspection_repository
            .tombstone_remote_inspection_prebind(input)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn gate_validated_remote_inspection_bind_v26(
        &self,
        input: BindRemoteInspectionPendingCommit<'_>,
    ) -> McpPlatformResult<
        crate::mcp_platform::intake_remote_inspection::RemoteInspectionReservationRecord,
    > {
        self.remote_inspection_repository
            .bind_remote_inspection_pending_commit(input)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn gate_validated_remote_inspection_commit_outcome_v26(
        &self,
        input: RecordRemoteInspectionCommitOutcome<'_>,
    ) -> McpPlatformResult<
        crate::mcp_platform::intake_remote_inspection::RemoteInspectionReservationRecord,
    > {
        self.remote_inspection_repository
            .record_remote_inspection_commit_outcome(input)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn gate_validated_remote_inspection_claim_v26(
        &self,
        input: ClaimCommittedRemoteInspectionAttempt<'_>,
    ) -> McpPlatformResult<
        crate::mcp_platform::intake_remote_inspection::RemoteInspectionAttemptRecord,
    > {
        self.remote_inspection_repository
            .claim_committed_remote_inspection_attempt(input)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn gate_validated_remote_inspection_owner_started_v26(
        &self,
        attempt_id: &str,
        claim_nonce: &str,
        now_ms: i64,
    ) -> McpPlatformResult<
        crate::mcp_platform::intake_remote_inspection::RemoteInspectionAttemptRecord,
    > {
        self.remote_inspection_repository
            .record_remote_inspection_owner_started(attempt_id, claim_nonce, now_ms)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn gate_validated_remote_inspection_owner_finished_v26(
        &self,
        attempt_id: &str,
        claim_nonce: &str,
        now_ms: i64,
    ) -> McpPlatformResult<
        crate::mcp_platform::intake_remote_inspection::RemoteInspectionAttemptRecord,
    > {
        self.remote_inspection_repository
            .record_remote_inspection_owner_finished(attempt_id, claim_nonce, now_ms)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn gate_validated_remote_inspection_finalize_v26(
        &self,
        input: FinalizeRemoteInspectionConsumption<'_>,
    ) -> McpPlatformResult<
        crate::mcp_platform::intake_remote_inspection::RemoteInspectionConsumptionRecord,
    > {
        self.remote_inspection_repository
            .finalize_remote_inspection_consumption(input)
            .await
    }

    pub async fn catalog_list(
        &self,
        _context: &RequestContext,
        input: CatalogListInput,
    ) -> McpPlatformResult<CatalogPage> {
        validate_optional_text(input.query.as_deref(), MAX_QUERY_LENGTH)?;
        validate_optional_text(input.cursor.as_deref(), MAX_CURSOR_LENGTH)?;
        for source_id in &input.source_ids {
            validate_identifier(source_id)?;
        }
        let page_size = validate_page_size(input.page_size)? as usize;
        let after = input.cursor.as_deref().map(decode_cursor).transpose()?;
        let normalized_query = input.query.as_deref().map(str::to_ascii_lowercase);
        let source_selected = input.source_ids.is_empty();

        let mut records = self.repository.list_manifests().await?;
        records.retain(|record| {
            !matches!(
                &record.source_metadata.source_ref,
                SourceRef::VerifiedSourceCatalog { .. }
            )
        });
        records.extend(
            self.repository
                .list_governed_catalog_entries()
                .await?
                .into_iter()
                .map(|entry| entry.manifest),
        );
        records.sort_by(|left, right| manifest_key(left).cmp(&manifest_key(right)));
        let matching_snapshot = records
            .into_iter()
            .filter(|record| {
                source_selected
                    || governed_source_id(&record.source_metadata.source_ref)
                        .is_ok_and(|source_id| input.source_ids.contains(&source_id))
            })
            .filter(|record| {
                input.trust_tiers.is_empty() || input.trust_tiers.contains(&record.trust_tier)
            })
            .filter(|record| {
                normalized_query.as_ref().is_none_or(|query| {
                    let manifest = record.verified.manifest();
                    manifest.id.to_ascii_lowercase().contains(query)
                        || manifest.name.to_ascii_lowercase().contains(query)
                        || manifest.description.to_ascii_lowercase().contains(query)
                })
            })
            .filter(|record| {
                input.compatibility.is_none_or(|expected| {
                    compatibility(&record.verified, self.options.compatibility_target) == expected
                })
            })
            .collect::<Vec<_>>();
        let newest_verified_at_ms = matching_snapshot
            .iter()
            .map(|record| record.created_at_ms)
            .max();
        let mut matching = matching_snapshot
            .into_iter()
            .filter(|record| {
                after
                    .as_ref()
                    .is_none_or(|cursor| manifest_key(record) > cursor.key())
            })
            .collect::<Vec<_>>();

        let has_more = matching.len() > page_size;
        matching.truncate(page_size);
        let next_cursor = has_more
            .then(|| matching.last().map(encode_cursor))
            .flatten()
            .transpose()?;
        let items = matching
            .into_iter()
            .map(|record| {
                let eligibility =
                    self.eligibility_for_manifest(&record.verified, record.trust_tier);
                catalog_summary(record, self.options.compatibility_target, eligibility)
            })
            .collect();

        Ok(CatalogPage {
            items,
            next_cursor,
            offline: true,
            local_persistence_only: true,
            newest_verified_at_ms,
        })
    }

    pub async fn catalog_detail(
        &self,
        _context: &RequestContext,
        locator: CatalogLocator,
    ) -> McpPlatformResult<CatalogDetail> {
        let record = match locator {
            CatalogLocator::ManifestDigest(digest) => {
                validate_digest(&digest)?;
                self.repository.get_manifest(&digest).await?
            }
            CatalogLocator::CatalogRef {
                source_id,
                mcp_id,
                version,
            } => {
                validate_identifier(&source_id)?;
                validate_identifier(&mcp_id)?;
                validate_identifier(&version)?;
                if source_id == LOCAL_PERSISTED_SOURCE_ID {
                    self.repository
                        .get_manifest_by_identity_with_source_context(
                            &mcp_id,
                            &version,
                            &ManifestSourceContext::PersistedManifest {
                                source_id: source_id.clone(),
                            },
                        )
                        .await?
                } else {
                    self.repository
                        .get_governed_catalog_entry(&source_id, &mcp_id, &version)
                        .await?
                        .manifest
                }
            }
        };
        let eligibility = self.eligibility_for_manifest(&record.verified, record.trust_tier);
        Ok(CatalogDetail {
            source_id: governed_source_id(&record.source_metadata.source_ref)?,
            manifest_digest: record.verified.digest().to_string(),
            proof: record.proof,
            trust_tier: record.trust_tier,
            source_metadata: record.source_metadata,
            compatibility: compatibility(&record.verified, self.options.compatibility_target),
            manifest: record.verified.manifest().clone(),
            verified_at_ms: record.created_at_ms,
            eligibility,
        })
    }

    pub async fn sources_policy_get(
        &self,
        _context: &RequestContext,
    ) -> McpPlatformResult<SourcePolicyState> {
        let mut records = self.repository.list_manifests().await?;
        records.retain(|record| {
            !matches!(
                &record.source_metadata.source_ref,
                SourceRef::VerifiedSourceCatalog { .. }
            )
        });
        records.extend(
            self.repository
                .list_governed_catalog_entries()
                .await?
                .into_iter()
                .map(|entry| entry.manifest),
        );
        let governed_sources = self.repository.list_governed_catalog_sources().await?;
        let governed_sources_by_id = governed_sources
            .into_iter()
            .map(|source| (source.source_id.clone(), source))
            .collect::<HashMap<_, _>>();
        let refreshes = self
            .repository
            .list_governed_source_refresh_registrations()
            .await?;
        let mut sources = HashMap::<String, SourceState>::new();
        for record in records {
            let source_id = governed_source_id(&record.source_metadata.source_ref)?;
            let source_ref = record.source_metadata.source_ref.clone();
            let source = sources
                .entry(source_id.clone())
                .or_insert_with(|| SourceState {
                    source_id: source_id.clone(),
                    display_name: governed_source_display_name(&source_ref),
                    import_kind: source_ref.import_kind(),
                    trust_tiers: Vec::new(),
                    manifest_count: 0,
                    last_imported_at_ms: None,
                    newest_verified_at_ms: None,
                    refreshable: false,
                    last_refreshed_at_ms: None,
                    last_refresh_document_digest: None,
                    last_refresh_result: None,
                    compatibility: CompatibilitySummary::default(),
                    recovery: RecoverySuggestion::None,
                });
            if let Some(governed) = governed_sources_by_id.get(&source_id) {
                source.display_name = governed.display_name.clone();
                source.import_kind = governed.import_kind;
            }
            source.manifest_count = source.manifest_count.saturating_add(1);
            if !source.trust_tiers.contains(&record.trust_tier) {
                source.trust_tiers.push(record.trust_tier);
                source.trust_tiers.sort_by_key(|tier| match tier {
                    crate::mcp_platform::TrustTier::Official => 0,
                    crate::mcp_platform::TrustTier::Community => 1,
                    crate::mcp_platform::TrustTier::Local => 2,
                });
            }
            source.last_imported_at_ms = Some(
                source
                    .last_imported_at_ms
                    .map_or(record.created_at_ms, |value| {
                        value.max(record.created_at_ms)
                    }),
            );
            source.newest_verified_at_ms = source.last_imported_at_ms;
            let outcome = if matches!(&source_ref, SourceRef::EnterpriseDirectory { .. }) {
                EligibilityOutcome::Restricted
            } else {
                self.eligibility_for_manifest(&record.verified, record.trust_tier)
                    .outcome
            };
            match outcome {
                EligibilityOutcome::Allowed => source.compatibility.compatible += 1,
                EligibilityOutcome::Restricted => source.compatibility.restricted += 1,
                EligibilityOutcome::Denied => source.compatibility.denied += 1,
            }
        }
        for refresh in refreshes {
            if let Some(source) = sources.get_mut(&refresh.source_id) {
                source.refreshable = true;
                source.last_refreshed_at_ms = refresh.last_refreshed_at_ms;
                source.last_refresh_document_digest = refresh.last_document_digest;
                source.last_refresh_result = Some(refresh.last_result);
            }
        }
        let mut sources = sources.into_values().collect::<Vec<_>>();
        sources.sort_by(|left, right| left.source_id.cmp(&right.source_id));
        Ok(SourcePolicyState {
            sources,
            policy: MachinePolicyState {
                target_platform: platform_name(self.options.compatibility_target.platform),
                target_architecture: architecture_name(self.options.compatibility_target.arch),
                development_mode: self.development_mode,
                docker_allowed: self.external_capabilities.docker_policy_allowed,
            },
        })
    }

    pub fn manual_stdio_sources_list(
        &self,
        _context: &RequestContext,
    ) -> McpPlatformResult<ManualStdioSourcesPage> {
        let items = self.manual_stdio_provider.list_sources()?;
        Ok(ManualStdioSourcesPage {
            available: !items.is_empty(),
            items,
        })
    }

    pub async fn manual_plan_create(
        &self,
        context: &RequestContext,
        input: ManualPlanCreateInput,
    ) -> McpPlatformResult<PlanReview> {
        validate_idempotency_key(&input.idempotency_key)?;
        let resolved = match input.connection {
            ManualConnectionInput::RemoteHttp { endpoint, auth } => {
                self.remote_http_network_policy
                    .validate_endpoint(&endpoint)?;
                if !matches!(auth, ManualHttpAuth::None) {
                    return Err(credential_missing());
                }
                self.remote_http_network_policy
                    .validate_for_plan(&endpoint, std::time::Duration::from_secs(30))
                    .await?;
                let verified =
                    manual_http_manifest(&manual_connection_id(&input.idempotency_key), &endpoint)?;
                super::dependencies::ResolvedManualStdioSource {
                    verified,
                    proof: crate::mcp_platform::ManifestProof::LocalBytes,
                    trust_tier: crate::mcp_platform::TrustTier::Local,
                    source_metadata: ManifestSourceMetadata::local_persistence(),
                }
            }
            ManualConnectionInput::StdioProvider { source_id } => {
                validate_identifier(&source_id)?;
                let resolved = self.manual_stdio_provider.resolve(&source_id)?;
                if !matches!(
                    resolved.verified.manifest().distribution,
                    Distribution::ManualStdio { .. }
                ) {
                    return Err(integrity_error());
                }
                resolved
            }
        };
        let digest = resolved.verified.digest().to_string();
        if let Some(existing) = self
            .repository
            .get_plan_by_idempotency_key(&input.idempotency_key)
            .await?
        {
            if existing.plan.manifest_digest() == digest {
                return self.review_for_plan(existing).await;
            }
            return Err(idempotency_conflict());
        }
        self.repository
            .save_manifest(&ManifestRecord {
                verified: resolved.verified,
                proof: resolved.proof,
                trust_tier: resolved.trust_tier,
                source_metadata: resolved.source_metadata,
                created_at_ms: self.clock.now_ms(),
            })
            .await?;
        self.plan_create(
            context,
            PlanCreateInput {
                intent: PlanIntent::Register {
                    manifest_digest: digest,
                    installation_scope: super::dto::InstallationScope::User,
                },
                idempotency_key: input.idempotency_key,
            },
        )
        .await
    }

    pub async fn plan_create(
        &self,
        context: &RequestContext,
        input: PlanCreateInput,
    ) -> McpPlatformResult<PlanReview> {
        self.plan_create_with_trusted_manifest(context, input, None)
            .await
    }

    async fn plan_create_with_trusted_manifest(
        &self,
        context: &RequestContext,
        input: PlanCreateInput,
        trusted: Option<(ManifestRecord, ManifestSourceContext)>,
    ) -> McpPlatformResult<PlanReview> {
        validate_idempotency_key(&input.idempotency_key)?;
        let (manifest, installation_scope, managed_mcp_id, operation, preserve_user_data) =
            match &input.intent {
                PlanIntent::Register {
                    manifest_digest,
                    installation_scope,
                } => {
                    validate_digest(manifest_digest)?;
                    (
                        match trusted.as_ref() {
                            Some((manifest, ManifestSourceContext::HttpsProvision { binding }))
                                if manifest.verified.digest() == manifest_digest
                                    && binding.parsed_digest == *manifest_digest =>
                            {
                                manifest.clone()
                            }
                            Some(_) => return Err(integrity_error()),
                            None => self.repository.get_manifest(manifest_digest).await?,
                        },
                        installation_scope.as_str().to_string(),
                        None,
                        PlanOperation::Register,
                        false,
                    )
                }
                PlanIntent::RegisterCatalog {
                    catalog,
                    installation_scope,
                } => {
                    validate_identifier(&catalog.source_id)?;
                    validate_identifier(&catalog.mcp_id)?;
                    validate_identifier(&catalog.version)?;
                    validate_digest(&catalog.manifest_digest)?;
                    let entry = self
                        .repository
                        .get_governed_catalog_entry(
                            &catalog.source_id,
                            &catalog.mcp_id,
                            &catalog.version,
                        )
                        .await?;
                    if entry.manifest.verified.digest() != catalog.manifest_digest {
                        return Err(integrity_error());
                    }
                    (
                        entry.manifest,
                        installation_scope.as_str().to_string(),
                        None,
                        PlanOperation::Register,
                        false,
                    )
                }
                PlanIntent::Install { manifest_digest } => {
                    validate_digest(manifest_digest)?;
                    let manifest = self.repository.get_manifest(manifest_digest).await?;
                    let managed_mcp_id = crate::mcp_platform::task_runner::stable_managed_mcp_id(
                        &manifest.verified.manifest().id,
                        "user",
                    );
                    (
                        manifest,
                        "user".to_string(),
                        Some(managed_mcp_id),
                        PlanOperation::Install,
                        false,
                    )
                }
                PlanIntent::InstallCatalog { catalog } => {
                    validate_identifier(&catalog.source_id)?;
                    validate_identifier(&catalog.mcp_id)?;
                    validate_identifier(&catalog.version)?;
                    validate_digest(&catalog.manifest_digest)?;
                    let entry = self
                        .repository
                        .get_governed_catalog_entry(
                            &catalog.source_id,
                            &catalog.mcp_id,
                            &catalog.version,
                        )
                        .await?;
                    if entry.manifest.verified.digest() != catalog.manifest_digest {
                        return Err(integrity_error());
                    }
                    let managed_mcp_id = crate::mcp_platform::task_runner::stable_managed_mcp_id(
                        &entry.manifest.verified.manifest().id,
                        "user",
                    );
                    (
                        entry.manifest,
                        "user".to_string(),
                        Some(managed_mcp_id),
                        PlanOperation::Install,
                        false,
                    )
                }
                PlanIntent::Update {
                    managed_mcp_id,
                    target_version,
                } => {
                    validate_identifier(managed_mcp_id)?;
                    ExactVersion::parse(target_version)?;
                    let inventory = self
                        .repository
                        .get_managed_inventory(managed_mcp_id)
                        .await?;
                    let manifest = self
                        .repository
                        .get_manifest_by_identity(&inventory.managed.mcp_id, target_version)
                        .await?;
                    (
                        manifest,
                        inventory.managed.installation_scope,
                        Some(managed_mcp_id.clone()),
                        PlanOperation::Update,
                        false,
                    )
                }
                PlanIntent::Repair { managed_mcp_id } => {
                    validate_identifier(managed_mcp_id)?;
                    let inventory = self
                        .repository
                        .get_managed_inventory(managed_mcp_id)
                        .await?;
                    let digest = inventory
                        .lifecycle
                        .active_manifest_digest
                        .as_deref()
                        .ok_or_else(integrity_error)?;
                    (
                        self.repository.get_manifest(digest).await?,
                        inventory.managed.installation_scope,
                        Some(managed_mcp_id.clone()),
                        PlanOperation::Repair,
                        false,
                    )
                }
                PlanIntent::Uninstall {
                    managed_mcp_id,
                    preserve_user_data,
                } => {
                    validate_identifier(managed_mcp_id)?;
                    let inventory = self
                        .repository
                        .get_managed_inventory(managed_mcp_id)
                        .await?;
                    let digest = inventory
                        .lifecycle
                        .active_manifest_digest
                        .as_deref()
                        .ok_or_else(integrity_error)?;
                    (
                        self.repository.get_manifest(digest).await?,
                        inventory.managed.installation_scope,
                        Some(managed_mcp_id.clone()),
                        PlanOperation::Uninstall,
                        *preserve_user_data,
                    )
                }
            };
        if let Transport::StreamableHttp {
            url,
            connect_timeout_seconds,
            allowed_redirect_origins,
        } = &manifest.verified.manifest().transport
        {
            self.remote_http_network_policy.validate_endpoint(url)?;
            if !matches!(manifest.verified.manifest().auth, Auth::None) {
                return Err(credential_missing());
            }
            if !allowed_redirect_origins.is_empty() {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::UnsafeUrl,
                    "managed remote HTTP redirects are not supported",
                ));
            }
            self.remote_http_network_policy
                .validate_for_plan(
                    url,
                    std::time::Duration::from_secs(connect_timeout_seconds.unwrap_or(30)),
                )
                .await?;
        }
        let policy_context = PolicyContext::new(manifest.trust_tier, operation)
            .with_target(
                self.options.compatibility_target.platform,
                self.options.compatibility_target.arch,
            )
            .with_runtime_capabilities(
                self.runtime_capabilities.node_available,
                self.runtime_capabilities.python_major_minor,
            )
            .with_external_capabilities(
                self.external_capabilities.docker_available,
                self.external_capabilities.git_available,
                self.development_mode,
            )
            .with_docker_policy(self.external_capabilities.docker_policy_allowed);
        let mut plan = if operation == PlanOperation::Uninstall {
            let managed_mcp_id = managed_mcp_id.as_deref().ok_or_else(integrity_error)?;
            let projection = self
                .repository
                .get_connection_projection(managed_mcp_id)
                .await?;
            uninstall_plan(
                &manifest.verified,
                manifest.trust_tier,
                managed_mcp_id,
                preserve_user_data,
                projection.projection,
                &policy_context,
            )?
        } else {
            plan_for_manifest(&manifest.verified, &policy_context)?
        };
        let now_ms = self.clock.now_ms();
        let expires_at_ms = now_ms
            .checked_add(self.options.plan_ttl_ms)
            .ok_or_else(invalid_request)?;
        let target = PlanTarget {
            managed_mcp_id,
            mcp_id: plan.manifest_id().to_string(),
            version: plan.manifest_version().to_string(),
            installation_scope: Some(installation_scope.clone()),
            source_context: Some(match trusted.as_ref() {
                Some((_, source_context)) => source_context.clone(),
                None => source_context_for_metadata(&manifest.source_metadata)?,
            }),
        };
        if let Some(source_context) = target.source_context.clone() {
            plan.bind_source_context(source_context)?;
        }
        if let Some(existing) = self
            .repository
            .get_plan_by_idempotency_key(&input.idempotency_key)
            .await?
        {
            ensure_plan_envelope_matches(&existing, &plan, &target)?;
            return self.review_for_plan(existing).await;
        }
        let save = SavePlan {
            plan_id: &self.ids.next_id("plan"),
            idempotency_key: &input.idempotency_key,
            plan: &plan,
            target: &target,
            policy_evidence: plan.policy(),
            confirmation_evidence: &ConfirmationEvidence::Pending,
            expires_at_ms,
            created_at_ms: now_ms,
            actor: context.actor(),
        };
        let stored = match self.repository.save_plan(save).await {
            Ok(stored) => stored,
            Err(error) if error.code() == McpPlatformErrorCode::PlanConflict => {
                let existing = self
                    .repository
                    .get_plan_by_idempotency_key(&input.idempotency_key)
                    .await?
                    .ok_or(error)?;
                ensure_plan_envelope_matches(&existing, &plan, &target)?;
                existing
            }
            Err(error) => return Err(error),
        };
        self.review_for_plan(stored).await
    }

    pub async fn install_confirm(
        &self,
        context: &RequestContext,
        input: InstallConfirmInput,
    ) -> McpPlatformResult<TaskRef> {
        validate_identifier(&input.plan_id)?;
        validate_digest(&input.plan_digest)?;
        validate_idempotency_key(&input.idempotency_key)?;
        let plan = self.repository.get_plan(&input.plan_id).await?;
        if plan.plan.plan_digest() != input.plan_digest {
            return Err(plan_stale());
        }

        if let Some(existing) = self
            .repository
            .get_task_by_idempotency_key(
                TaskOperation::from(plan.plan.operation()),
                &input.idempotency_key,
            )
            .await?
        {
            if existing.plan_id != input.plan_id || existing.plan_digest != input.plan_digest {
                return Err(idempotency_conflict());
            }
            match self.confirmation_decision(&existing).await? {
                Some(decision) if decision == input.decision => return Ok(existing.into()),
                Some(_) => return Err(idempotency_conflict()),
                None => {
                    let settled = self
                        .settle_confirmation(context, existing, input.decision)
                        .await?;
                    return Ok(settled.into());
                }
            }
        }
        if self.clock.now_ms() >= plan.expires_at_ms {
            return Err(plan_expired());
        }

        let task_id = self.ids.next_id("task");
        let adapter_evidence = AdapterEvidence {
            adapter_id: plan.plan.adapter().id.clone(),
            adapter_version: plan.plan.adapter().version.clone(),
            compatible_for_recovery: true,
            resume_safe: true,
        };
        let rollback_evidence = RollbackEvidence {
            compensation_available: true,
            remaining_compensations: vec![CompensationDescriptor::RemoveConnectionProjection {
                link_key: "server_owned_at_execution".to_string(),
            }],
        };
        let created = self
            .repository
            .create_task(CreateTask {
                task_id: &task_id,
                plan_id: &input.plan_id,
                plan_digest: &input.plan_digest,
                operation: TaskOperation::from(plan.plan.operation()),
                idempotency_key: &input.idempotency_key,
                actor: context.actor(),
                now_ms: self.clock.now_ms(),
                adapter_evidence: Some(&adapter_evidence),
                rollback_evidence: Some(&rollback_evidence),
            })
            .await?;
        let settled = self
            .settle_confirmation(context, created, input.decision)
            .await?;
        let task_ref = settled.into();
        self.runner.notify();
        Ok(task_ref)
    }

    pub async fn task_get(
        &self,
        _context: &RequestContext,
        input: TaskGetInput,
    ) -> McpPlatformResult<TaskRef> {
        validate_identifier(&input.task_id)?;
        self.repository
            .get_task(&input.task_id)
            .await
            .map(Into::into)
    }

    pub async fn task_cancel(
        &self,
        context: &RequestContext,
        input: TaskCancelInput,
    ) -> McpPlatformResult<TaskRef> {
        validate_identifier(&input.task_id)?;
        validate_revision(input.expected_revision)?;
        let task = self
            .repository
            .request_cancel(
                &input.task_id,
                input.expected_revision,
                context.actor(),
                self.clock.now_ms(),
            )
            .await
            .map(TaskRef::from)?;
        self.runner.cancel(&input.task_id).await;
        Ok(task)
    }

    pub async fn task_retry(
        &self,
        context: &RequestContext,
        input: TaskRetryInput,
    ) -> McpPlatformResult<TaskRef> {
        validate_identifier(&input.task_id)?;
        validate_revision(input.expected_revision)?;
        validate_idempotency_key(&input.idempotency_key)?;
        let task = self
            .repository
            .retry_task(
                &input.task_id,
                input.expected_revision,
                &input.idempotency_key,
                context.actor(),
                self.clock.now_ms(),
            )
            .await
            .map(TaskRef::from)?;
        self.runner.notify();
        Ok(task)
    }

    pub async fn managed_list(
        &self,
        _context: &RequestContext,
        input: ManagedListInput,
    ) -> McpPlatformResult<ManagedMcpPage> {
        validate_optional_text(input.cursor.as_deref(), MAX_CURSOR_LENGTH)?;
        let page_size = validate_page_size(input.page_size)? as usize;
        let records = self
            .repository
            .list_managed_inventory(
                input.cursor.as_deref(),
                page_size + 1,
                &ManagedInventoryFilter {
                    registration: input.registration,
                    installation: input.installation,
                    runtime: input.runtime,
                    health: input.health,
                    default_enabled: input.default_enabled,
                },
            )
            .await?;
        let available = available_managed_manifests(
            self.repository.list_manifests().await?,
            self.options.compatibility_target,
            self.runtime_capabilities,
            self.external_capabilities,
            self.development_mode,
        );
        let has_more = records.len() > page_size;
        let scanned_cursor =
            has_more.then(|| records[page_size - 1].managed.managed_mcp_id.clone());
        let mut items = Vec::with_capacity(page_size.min(records.len()));
        for record in records.into_iter().take(page_size) {
            let task = self
                .repository
                .latest_managed_lifecycle_task(&record.managed.managed_mcp_id)
                .await?;
            let projection_recovery = self
                .repository
                .projection_recovery_required(&record.managed.managed_mcp_id)
                .await?;
            let available_manifest = available
                .get(&(
                    record.managed.mcp_id.clone(),
                    record.lifecycle.distribution_adapter.clone(),
                ))
                .cloned();
            let credential_status = self
                .managed_credential_status_for_inventory(&record)
                .await?;
            items.push(managed_summary(
                record,
                task,
                projection_recovery,
                available_manifest,
                self.external_capabilities,
                credential_status,
            ));
        }
        Ok(ManagedMcpPage {
            items,
            next_cursor: scanned_cursor,
        })
    }

    #[cfg(feature = "integration-test-support")]
    pub async fn managed_list_diagnostic_stage_for_integration(
        &self,
        page_size: u16,
    ) -> &'static str {
        let page_size = usize::from(page_size.clamp(1, MAX_PAGE_SIZE));
        let records = match self
            .repository
            .list_managed_inventory(None, page_size + 1, &ManagedInventoryFilter::default())
            .await
        {
            Ok(records) => records,
            Err(_) => return "inventory",
        };
        if self.repository.list_manifests().await.is_err() {
            return "manifests";
        }

        for record in records.into_iter().take(page_size) {
            if self
                .repository
                .latest_managed_lifecycle_task(&record.managed.managed_mcp_id)
                .await
                .is_err()
            {
                return "latest_task";
            }
            if self
                .repository
                .projection_recovery_required(&record.managed.managed_mcp_id)
                .await
                .is_err()
            {
                return "projection_recovery";
            }
            if self
                .managed_credential_status_for_inventory(&record)
                .await
                .is_err()
            {
                return "credential_status";
            }
        }

        "complete"
    }

    pub async fn managed_get(
        &self,
        _context: &RequestContext,
        input: ManagedGetInput,
    ) -> McpPlatformResult<ManagedMcpDetail> {
        validate_identifier(&input.managed_mcp_id)?;
        let inventory = self
            .repository
            .get_managed_inventory(&input.managed_mcp_id)
            .await?;
        let projection = self
            .repository
            .get_connection_projection(&input.managed_mcp_id)
            .await?;
        let latest_health = self
            .repository
            .latest_health_observation(&input.managed_mcp_id)
            .await?;
        let latest_lifecycle_task = self
            .repository
            .latest_managed_lifecycle_task(&input.managed_mcp_id)
            .await?;
        let projection_recovery = self
            .repository
            .projection_recovery_required(&input.managed_mcp_id)
            .await?;
        let registration_task = match inventory.lifecycle.owner_task_id.as_deref() {
            Some(task_id) => Some(self.repository.get_task(task_id).await?.into()),
            None => None,
        };
        let available_manifest = available_managed_manifests(
            self.repository.list_manifests().await?,
            self.options.compatibility_target,
            self.runtime_capabilities,
            self.external_capabilities,
            self.development_mode,
        )
        .remove(&(
            inventory.managed.mcp_id.clone(),
            inventory.lifecycle.distribution_adapter.clone(),
        ));
        let manifest = inventory
            .lifecycle
            .active_manifest_digest
            .as_deref()
            .ok_or_else(integrity_error)?;
        let manifest = self.repository.get_manifest(manifest).await?;
        let credential_status = managed_credential_status(&manifest.verified.manifest().auth);
        let source_metadata = manifest.source_metadata;
        let supply_chain = inventory
            .managed
            .versions
            .iter()
            .find(|version| version.active)
            .and_then(|version| version.supply_chain_evidence.as_ref())
            .map(supply_chain_summary);
        Ok(ManagedMcpDetail {
            summary: managed_summary(
                inventory.clone(),
                latest_lifecycle_task,
                projection_recovery,
                available_manifest,
                self.external_capabilities,
                credential_status,
            ),
            distribution_adapter: inventory.lifecycle.distribution_adapter,
            active_manifest_digest: inventory
                .lifecycle
                .active_manifest_digest
                .ok_or_else(integrity_error)?,
            active_version: inventory
                .lifecycle
                .active_version
                .ok_or_else(integrity_error)?,
            source_metadata,
            extension_config_key: projection.link_key,
            projection_digest: projection.projection_digest,
            latest_health,
            registration_task,
            supply_chain,
        })
    }

    pub async fn managed_runtime_control(
        &self,
        _context: &RequestContext,
        input: ManagedRuntimeControlInput,
        runtime_control: &dyn ManagedRuntimeControlPort,
    ) -> McpPlatformResult<ManagedMcpSummary> {
        validate_identifier(&input.managed_mcp_id)?;
        validate_revision(input.expected_revision)?;
        let managed_mcp_id = input.managed_mcp_id.clone();
        let mut inventory = self
            .repository
            .get_managed_inventory(&input.managed_mcp_id)
            .await?;
        if inventory.managed.state.registration
            != crate::mcp_platform::RegistrationState::Registered
        {
            return Err(invalid_transition());
        }
        if inventory.managed.revision != input.expected_revision {
            return Err(revision_conflict());
        }
        let can_transition = match input.action {
            ManagedRuntimeAction::Start => matches!(
                inventory.managed.state.runtime,
                crate::mcp_platform::RuntimeState::Stopped
                    | crate::mcp_platform::RuntimeState::Crashed
            ),
            ManagedRuntimeAction::Stop => matches!(
                inventory.managed.state.runtime,
                crate::mcp_platform::RuntimeState::Running
                    | crate::mcp_platform::RuntimeState::Crashed
            ),
        };
        if !can_transition {
            return Err(invalid_transition());
        }
        let latest_task = self
            .repository
            .latest_managed_lifecycle_task(&managed_mcp_id)
            .await?;
        if !latest_task
            .as_ref()
            .is_none_or(|task| task.status.is_terminal())
        {
            return Err(invalid_transition());
        }
        let projection_recovery = self
            .repository
            .projection_recovery_required(&managed_mcp_id)
            .await?;
        if projection_recovery {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::ProjectionConflict,
                "projection recovery is required",
            ));
        }
        let projection = self
            .repository
            .get_connection_projection(&managed_mcp_id)
            .await?;
        let command = ManagedRuntimeCommand {
            managed_mcp_id: managed_mcp_id.clone(),
            extension_key: projection.link_key.clone(),
            expected_config: CoreTransportProjectionAdapter
                .extension_config(&projection.projection, &projection.link_key)?,
            action: match input.action {
                ManagedRuntimeAction::Start => HostRuntimeAction::Start,
                ManagedRuntimeAction::Stop => HostRuntimeAction::Stop,
            },
        };
        let mut pre_transition = inventory.managed.state.clone();
        pre_transition.runtime = match input.action {
            ManagedRuntimeAction::Start => crate::mcp_platform::RuntimeState::Starting,
            ManagedRuntimeAction::Stop => crate::mcp_platform::RuntimeState::Stopping,
        };
        let pre_record = self
            .repository
            .update_managed_state(
                &managed_mcp_id,
                input.expected_revision,
                &pre_transition,
                self.clock.now_ms(),
            )
            .await?;

        let control_result = runtime_control.control(command).await;
        let desired = match control_result {
            Ok(true) => match input.action {
                ManagedRuntimeAction::Start => crate::mcp_platform::RuntimeState::Running,
                ManagedRuntimeAction::Stop => crate::mcp_platform::RuntimeState::Stopped,
            },
            Ok(false) | Err(_) => crate::mcp_platform::RuntimeState::Crashed,
        };
        let mut post_transition = pre_record.state.clone();
        post_transition.runtime = desired;
        self.repository
            .update_managed_state(
                &managed_mcp_id,
                pre_record.revision,
                &post_transition,
                self.clock.now_ms(),
            )
            .await?;

        match control_result {
            Ok(true) => {}
            Ok(false) => {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::RuntimeControlUnavailable,
                    "managed runtime control reported no owned runtime was changed",
                ));
            }
            Err(error) => return Err(error),
        }
        inventory = self
            .repository
            .get_managed_inventory(&managed_mcp_id)
            .await?;
        let available_manifest = available_managed_manifests(
            self.repository.list_manifests().await?,
            self.options.compatibility_target,
            self.runtime_capabilities,
            self.external_capabilities,
            self.development_mode,
        )
        .remove(&(
            inventory.managed.mcp_id.clone(),
            inventory.lifecycle.distribution_adapter.clone(),
        ));
        let credential_status = self
            .managed_credential_status_for_inventory(&inventory)
            .await?;
        let summary = managed_summary(
            inventory,
            latest_task,
            projection_recovery,
            available_manifest,
            self.external_capabilities,
            credential_status,
        );
        Ok(summary)
    }

    pub async fn health_run(
        &self,
        context: &RequestContext,
        input: HealthRunInput,
    ) -> McpPlatformResult<TaskRef> {
        validate_identifier(&input.managed_mcp_id)?;
        validate_idempotency_key(&input.idempotency_key)?;
        let inventory = self
            .repository
            .get_managed_inventory(&input.managed_mcp_id)
            .await?;
        if inventory.managed.state.registration
            != crate::mcp_platform::RegistrationState::Registered
        {
            return Err(invalid_transition());
        }
        let task = self
            .repository
            .create_health_task(CreateHealthTask {
                task_id: &self.ids.next_id("task"),
                managed_mcp_id: &input.managed_mcp_id,
                mode: input.mode,
                idempotency_key: &input.idempotency_key,
                actor: context.actor(),
                now_ms: self.clock.now_ms(),
            })
            .await?;
        self.runner.notify();
        Ok(task.into())
    }

    pub async fn health_get(
        &self,
        _context: &RequestContext,
        input: HealthGetInput,
    ) -> McpPlatformResult<HealthStatus> {
        validate_identifier(&input.managed_mcp_id)?;
        let inventory = self
            .repository
            .get_managed_inventory(&input.managed_mcp_id)
            .await?;
        let credential_status = self
            .managed_credential_status_for_inventory(&inventory)
            .await?;
        Ok(HealthStatus {
            managed_mcp_id: input.managed_mcp_id.clone(),
            state: inventory.managed.state.health,
            credential_status,
            latest: self
                .repository
                .latest_health_observation(&input.managed_mcp_id)
                .await?,
        })
    }

    async fn managed_credential_status_for_inventory(
        &self,
        inventory: &ManagedMcpInventoryRecord,
    ) -> McpPlatformResult<Option<CredentialStatus>> {
        match inventory.lifecycle.active_manifest_digest.as_deref() {
            Some(digest) => {
                let manifest = self.repository.get_manifest(digest).await?;
                Ok(managed_credential_status(
                    &manifest.verified.manifest().auth,
                ))
            }
            None => Ok(None),
        }
    }

    pub async fn set_default_enabled(
        &self,
        _context: &RequestContext,
        input: SetDefaultEnabledInput,
    ) -> McpPlatformResult<ManagedMcpSummary> {
        validate_identifier(&input.managed_mcp_id)?;
        validate_revision(input.expected_revision)?;
        let managed_mcp_id = input.managed_mcp_id.clone();
        let record = self
            .runner
            .set_default_enabled(
                &input.managed_mcp_id,
                input.expected_revision,
                input.enabled,
            )
            .await?;
        let latest_task = self
            .repository
            .latest_managed_lifecycle_task(&managed_mcp_id)
            .await?;
        let projection_recovery = self
            .repository
            .projection_recovery_required(&managed_mcp_id)
            .await?;
        let available_manifest = available_managed_manifests(
            self.repository.list_manifests().await?,
            self.options.compatibility_target,
            self.runtime_capabilities,
            self.external_capabilities,
            self.development_mode,
        )
        .remove(&(
            record.managed.mcp_id.clone(),
            record.lifecycle.distribution_adapter.clone(),
        ));
        let credential_status = self
            .managed_credential_status_for_inventory(&record)
            .await?;
        Ok(managed_summary(
            record,
            latest_task,
            projection_recovery,
            available_manifest,
            self.external_capabilities,
            credential_status,
        ))
    }

    pub(crate) fn require_mutation_authority(&self) -> McpPlatformResult<()> {
        Ok(())
    }

    pub(crate) async fn validate_managed_plan_authority(
        &self,
        inventory: &ManagedMcpInventoryRecord,
    ) -> McpPlatformResult<()> {
        if self
            .repository
            .projection_recovery_required(&inventory.managed.managed_mcp_id)
            .await?
        {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::ProjectionConflict,
                "managed MCP projection requires recovery before plan use",
            ));
        }
        let Some(task_id) = inventory.lifecycle.owner_task_id.as_deref() else {
            return Ok(());
        };
        let task = self.repository.get_task(task_id).await?;
        if !task.status.is_terminal() {
            return Err(invalid_transition());
        }
        Ok(())
    }

    pub async fn events_resume(
        &self,
        _context: &RequestContext,
        input: EventsResumeInput,
    ) -> McpPlatformResult<EventsResumePage> {
        let after_event_id = input.after_event_id.unwrap_or_default();
        if after_event_id < 0 {
            return Err(invalid_request());
        }
        if input.task_ids.len() > MAX_TASK_FILTERS {
            return Err(invalid_request());
        }
        for task_id in &input.task_ids {
            validate_identifier(task_id)?;
        }
        let limit = validate_event_limit(input.limit)? as usize;
        let page = self
            .repository
            .list_global_audit_events(after_event_id, limit, &input.task_ids)
            .await?;
        let task_ids = unique_task_ids(&page.events, &input.task_ids);
        let mut tasks = Vec::with_capacity(task_ids.len());
        for task_id in task_ids {
            tasks.push(self.repository.get_task(&task_id).await?.into());
        }
        Ok(EventsResumePage {
            events: page.events,
            next_event_id: page.scanned_through_event_id.max(after_event_id),
            tasks,
        })
    }

    async fn review_for_plan(&self, plan: PlanRecord) -> McpPlatformResult<PlanReview> {
        let (manifest, _) = self.repository.resolve_manifest_for_plan(&plan).await?;
        let immutable_evidence = immutable_evidence(&plan.plan);
        let reversibility = plan_reversibility(&plan.plan);
        let recovery = policy_recovery(&plan.policy_evidence);
        let source_id = source_id_for_metadata(&manifest.source_metadata)?;
        Ok(PlanReview {
            plan_id: plan.plan_id,
            plan_digest: plan.plan.plan_digest().to_string(),
            expires_at_ms: plan.expires_at_ms,
            source_id: source_id.clone(),
            catalog_target: Some(CatalogPlanTarget {
                source_id,
                mcp_id: manifest.verified.manifest().id.clone(),
                version: manifest.verified.manifest().version.as_str().to_string(),
                manifest_digest: manifest.verified.digest().to_string(),
            }),
            proof: manifest.proof,
            trust_tier: manifest.trust_tier,
            source_metadata: manifest.source_metadata,
            manifest: manifest.verified.manifest().clone(),
            plan: plan.plan,
            target: plan.target,
            policy: plan.policy_evidence,
            immutable_evidence,
            reversibility,
            recovery,
        })
    }

    async fn settle_confirmation(
        &self,
        context: &RequestContext,
        mut task: crate::mcp_platform::repository::TaskRecord,
        decision: UserDecision,
    ) -> McpPlatformResult<crate::mcp_platform::repository::TaskRecord> {
        for _ in 0..8 {
            let result = match task.status {
                TaskStatus::Planned => {
                    self.repository
                        .transition_task(TaskTransition {
                            task_id: &task.task_id,
                            expected_revision: task.revision,
                            next_status: TaskStatus::AwaitingConfirmation,
                            actor: context.actor(),
                            now_ms: self.clock.now_ms(),
                            heartbeat_at_ms: None,
                            progress: 0,
                            redacted_error: None,
                            rollback_status: RollbackStatus::NotRequired,
                            rollback_evidence: None,
                        })
                        .await
                }
                TaskStatus::AwaitingConfirmation => match decision {
                    UserDecision::Confirm => {
                        self.repository
                            .confirm_task(
                                &task.task_id,
                                task.revision,
                                context.actor(),
                                self.clock.now_ms(),
                            )
                            .await
                    }
                    UserDecision::Reject => {
                        self.repository
                            .request_cancel(
                                &task.task_id,
                                task.revision,
                                context.actor(),
                                self.clock.now_ms(),
                            )
                            .await
                    }
                },
                TaskStatus::Queued if decision == UserDecision::Confirm => return Ok(task),
                TaskStatus::Cancelled if decision == UserDecision::Reject => return Ok(task),
                TaskStatus::Queued | TaskStatus::Cancelled => return Err(idempotency_conflict()),
                _ => return Err(invalid_transition()),
            };
            match result {
                Ok(updated) => task = updated,
                Err(error) if error.code() == McpPlatformErrorCode::RevisionConflict => {
                    task = self.repository.get_task(&task.task_id).await?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(revision_conflict())
    }

    async fn confirmation_decision(
        &self,
        task: &crate::mcp_platform::repository::TaskRecord,
    ) -> McpPlatformResult<Option<UserDecision>> {
        let events = self.repository.list_audit_events(&task.task_id).await?;
        let confirmed = events.iter().any(|event| {
            event.event_type
                == crate::mcp_platform::repository::AuditEventType::ConfirmationRecorded
        });
        let rejected = events.iter().any(|event| {
            event.event_type
                == crate::mcp_platform::repository::AuditEventType::CancellationRequested
                && !confirmed
        });
        match (confirmed, rejected) {
            (true, _) => Ok(Some(UserDecision::Confirm)),
            (false, true) => Ok(Some(UserDecision::Reject)),
            (false, false) => Ok(None),
        }
    }

    fn eligibility_for_manifest(
        &self,
        verified: &VerifiedManifest,
        trust_tier: crate::mcp_platform::TrustTier,
    ) -> Eligibility {
        self.eligibility_for_manifest_with_source_confirmation(verified, trust_tier, false)
    }

    fn eligibility_for_verified_https_manifest(&self, verified: &VerifiedManifest) -> Eligibility {
        self.eligibility_for_manifest_with_source_confirmation(
            verified,
            crate::mcp_platform::TrustTier::Local,
            true,
        )
    }

    fn eligibility_for_manifest_with_source_confirmation(
        &self,
        verified: &VerifiedManifest,
        trust_tier: crate::mcp_platform::TrustTier,
        verified_https_provision: bool,
    ) -> Eligibility {
        if let Transport::StreamableHttp {
            url,
            allowed_redirect_origins,
            ..
        } = &verified.manifest().transport
        {
            if !allowed_redirect_origins.is_empty()
                || !matches!(verified.manifest().auth, Auth::None)
                || self
                    .remote_http_network_policy
                    .validate_endpoint(url)
                    .is_err()
            {
                return Eligibility {
                    outcome: EligibilityOutcome::Denied,
                    reason: EligibilityReason::PolicyDenied,
                    recovery: RecoverySuggestion::ContactPolicyAdministrator,
                };
            }
        }
        let compatible = compatibility(verified, self.options.compatibility_target)
            == CatalogCompatibility::Compatible;
        if !compatible {
            return Eligibility {
                outcome: EligibilityOutcome::Denied,
                reason: EligibilityReason::PlatformUnsupported,
                recovery: RecoverySuggestion::ChooseCompatibleRelease,
            };
        }
        let operation = match verified.manifest().distribution {
            Distribution::RemoteHttp | Distribution::ManualStdio { .. } => PlanOperation::Register,
            _ => PlanOperation::Install,
        };
        let mut context = PolicyContext::new(trust_tier, operation)
            .with_target(
                self.options.compatibility_target.platform,
                self.options.compatibility_target.arch,
            )
            .with_runtime_capabilities(
                self.runtime_capabilities.node_available,
                self.runtime_capabilities.python_major_minor,
            )
            .with_external_capabilities(
                self.external_capabilities.docker_available,
                self.external_capabilities.git_available,
                self.development_mode,
            )
            .with_docker_policy(self.external_capabilities.docker_policy_allowed);
        if verified_https_provision {
            context = context.with_verified_https_provision_source_confirmation();
        }
        match plan_for_manifest(verified, &context) {
            Ok(plan)
                if plan.policy().outcome == crate::mcp_platform::policy::PolicyOutcome::Allow =>
            {
                Eligibility {
                    outcome: EligibilityOutcome::Allowed,
                    reason: EligibilityReason::Eligible,
                    recovery: RecoverySuggestion::None,
                }
            }
            Ok(_) => Eligibility {
                outcome: EligibilityOutcome::Restricted,
                reason: EligibilityReason::ConfirmationRequired,
                recovery: RecoverySuggestion::ReviewPermissions,
            },
            Err(error) => eligibility_from_error(error.code()),
        }
    }
}

fn supply_chain_summary(
    evidence: &crate::mcp_platform::SupplyChainEvidence,
) -> ManagedSupplyChainSummary {
    match evidence {
        crate::mcp_platform::SupplyChainEvidence::Docker {
            image,
            image_digest,
            adapter_version,
            daemon_version,
            rootless,
            mount_plan_digest,
            created_at_ms,
        } => ManagedSupplyChainSummary::Docker {
            image: image.clone(),
            image_digest: image_digest.clone(),
            adapter_version: adapter_version.clone(),
            daemon_version: daemon_version.clone(),
            rootless: *rootless,
            mount_plan_digest: mount_plan_digest.clone(),
            created_at_ms: *created_at_ms,
        },
        crate::mcp_platform::SupplyChainEvidence::GitDev {
            repository_origin,
            commit,
            git_tree_id,
            materialized_tree_digest,
            adapter_version,
            created_at_ms,
        } => ManagedSupplyChainSummary::GitDev {
            repository_origin: redact_safe_origin(repository_origin),
            commit: commit.clone(),
            git_tree_id: git_tree_id.clone(),
            materialized_tree_digest: materialized_tree_digest.clone(),
            adapter_version: adapter_version.clone(),
            created_at_ms: *created_at_ms,
        },
    }
}

fn redact_safe_origin(value: &str) -> String {
    Url::parse(value)
        .ok()
        .map(|mut url| {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        })
        .unwrap_or_else(|| "redacted-origin".to_string())
}

fn immutable_evidence(plan: &InstallationPlan) -> ImmutableEvidence {
    for step in plan.steps() {
        match step {
            PlanStep::AcquireManagedDistribution {
                artifact_digest,
                expected_size_bytes,
                ..
            } => {
                return ImmutableEvidence::Artifact {
                    sha256: artifact_digest.value.clone(),
                    size_bytes: *expected_size_bytes,
                };
            }
            PlanStep::AcquireDockerDistribution { image, digest, .. } => {
                return ImmutableEvidence::Docker {
                    image: image.clone(),
                    image_digest: digest.value.clone(),
                };
            }
            PlanStep::AcquireGitDevDistribution {
                repository_origin,
                commit,
                ..
            } => {
                return ImmutableEvidence::GitDev {
                    repository_origin: redact_safe_origin(repository_origin),
                    commit: commit.clone(),
                    tree: EvidenceValue::Unavailable(
                        EvidenceUnavailableReason::AvailableAfterMaterialization,
                    ),
                    materialized_digest: EvidenceValue::Unavailable(
                        EvidenceUnavailableReason::AvailableAfterMaterialization,
                    ),
                };
            }
            _ => {}
        }
    }
    ImmutableEvidence::Unavailable {
        reason: EvidenceUnavailableReason::NoArtifactForRegistration,
    }
}

fn plan_reversibility(plan: &InstallationPlan) -> PlanReversibility {
    match plan.operation() {
        PlanOperation::Register => PlanReversibility {
            reversible: true,
            strategy: RollbackStrategy::RemoveConnectionRegistration,
        },
        PlanOperation::Install | PlanOperation::Update => PlanReversibility {
            reversible: true,
            strategy: RollbackStrategy::StagedActivationRestoresPreviousVersion,
        },
        PlanOperation::Repair => PlanReversibility {
            reversible: true,
            strategy: RollbackStrategy::RepairRestoresVerifiedOwnedContent,
        },
        PlanOperation::Uninstall => {
            let strategy = plan.steps().iter().find_map(|step| match step {
                PlanStep::RemoveManagedInstallation {
                    preserve_user_data, ..
                } => Some(RollbackStrategy::UninstallRemovesOwnedFiles {
                    preserve_user_data: *preserve_user_data,
                }),
                _ => None,
            });
            PlanReversibility {
                reversible: false,
                strategy: strategy.unwrap_or(RollbackStrategy::Unavailable),
            }
        }
        PlanOperation::Health => PlanReversibility {
            reversible: false,
            strategy: RollbackStrategy::Unavailable,
        },
    }
}

fn policy_recovery(decision: &crate::mcp_platform::policy::PolicyDecision) -> RecoverySuggestion {
    if decision.outcome == crate::mcp_platform::policy::PolicyOutcome::Allow {
        RecoverySuggestion::None
    } else if decision.outcome == crate::mcp_platform::policy::PolicyOutcome::NeedsConfirmation {
        RecoverySuggestion::ReviewPermissions
    } else if decision.reasons.iter().any(|reason| {
        reason.code == crate::mcp_platform::policy::PolicyReasonCode::DevelopmentModeRequired
    }) {
        RecoverySuggestion::EnableDevelopmentMode
    } else {
        RecoverySuggestion::ContactPolicyAdministrator
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogCursor {
    mcp_id: String,
    version: String,
    digest: String,
}

impl CatalogCursor {
    fn key(&self) -> (&str, &str, &str) {
        (&self.mcp_id, &self.version, &self.digest)
    }
}

fn encode_cursor(
    record: &crate::mcp_platform::repository::ManifestRecord,
) -> McpPlatformResult<String> {
    let cursor = CatalogCursor {
        mcp_id: record.verified.manifest().id.clone(),
        version: record.verified.manifest().version.as_str().to_string(),
        digest: record.verified.digest().to_string(),
    };
    serde_json::to_vec(&cursor)
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|_| invalid_request())
}

fn decode_cursor(value: &str) -> McpPlatformResult<CatalogCursor> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| invalid_request())?;
    let cursor: CatalogCursor = serde_json::from_slice(&bytes).map_err(|_| invalid_request())?;
    validate_identifier(&cursor.mcp_id)?;
    validate_identifier(&cursor.version)?;
    validate_digest(&cursor.digest)?;
    Ok(cursor)
}

fn catalog_summary(
    record: crate::mcp_platform::repository::ManifestRecord,
    target: CompatibilityTarget,
    eligibility: Eligibility,
) -> CatalogSummary {
    let manifest = record.verified.manifest();
    CatalogSummary {
        source_id: LOCAL_PERSISTED_SOURCE_ID.to_string(),
        manifest_digest: record.verified.digest().to_string(),
        mcp_id: manifest.id.clone(),
        version: manifest.version.as_str().to_string(),
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        publisher_id: manifest.publisher.id.clone(),
        publisher_name: manifest.publisher.name.clone(),
        trust_tier: record.trust_tier,
        proof: record.proof,
        source_metadata: record.source_metadata,
        compatibility: compatibility(&record.verified, target),
        distribution_adapter: manifest.distribution.adapter_id().to_string(),
        verified_at_ms: record.created_at_ms,
        eligibility,
    }
}

fn managed_summary(
    record: ManagedMcpInventoryRecord,
    latest_task: Option<TaskRecord>,
    projection_recovery: bool,
    available_manifests: Option<AvailableManagedManifests>,
    external: ExternalCapabilitySnapshot,
    credential_status: Option<CredentialStatus>,
) -> ManagedMcpSummary {
    let external_capability = match record.lifecycle.distribution_adapter.as_str() {
        "docker" => Some(if !external.docker_available {
            super::dto::ManagedExternalCapabilityStatus::DockerCliMissing
        } else if !external.docker_policy_allowed {
            super::dto::ManagedExternalCapabilityStatus::DockerDaemonPolicyDenied
        } else if external.docker_daemon_verified {
            super::dto::ManagedExternalCapabilityStatus::DockerDaemonVerified
        } else {
            super::dto::ManagedExternalCapabilityStatus::DockerDaemonUnverified
        }),
        "git_dev" => Some(if external.git_available {
            super::dto::ManagedExternalCapabilityStatus::GitAvailable
        } else {
            super::dto::ManagedExternalCapabilityStatus::GitMissing
        }),
        _ => None,
    };
    let managed_distribution = matches!(
        record.lifecycle.distribution_adapter.as_str(),
        "npm" | "python_wheel" | "binary_archive" | "docker" | "git_dev"
    );
    let installed = matches!(
        record.managed.state.installation,
        crate::mcp_platform::InstallationState::Installed
            | crate::mcp_platform::InstallationState::UpdateAvailable
            | crate::mcp_platform::InstallationState::RepairRequired
    );
    let recovery_required = projection_recovery
        || latest_task.as_ref().is_some_and(|task| {
            task.status == crate::mcp_platform::TaskStatus::RecoveryRequired
                || task.rollback_status == crate::mcp_platform::RollbackStatus::Incomplete
        });
    let interrupted = latest_task
        .as_ref()
        .is_some_and(|task| task.status == crate::mcp_platform::TaskStatus::Interrupted);
    let in_progress = latest_task.as_ref().is_some_and(|task| {
        matches!(
            task.status,
            crate::mcp_platform::TaskStatus::Planned
                | crate::mcp_platform::TaskStatus::AwaitingConfirmation
                | crate::mcp_platform::TaskStatus::Queued
                | crate::mcp_platform::TaskStatus::Running
                | crate::mcp_platform::TaskStatus::Cancelling
                | crate::mcp_platform::TaskStatus::Verifying
                | crate::mcp_platform::TaskStatus::Activating
                | crate::mcp_platform::TaskStatus::RollingBack
        )
    });
    let current_task = latest_task
        .filter(|_task| recovery_required || interrupted || in_progress)
        .map(Into::into);
    let reason = if recovery_required {
        super::dto::ManagedEligibilityReason::TaskRecoveryRequired
    } else if interrupted {
        super::dto::ManagedEligibilityReason::TaskInterrupted
    } else if in_progress {
        super::dto::ManagedEligibilityReason::TaskInProgress
    } else if !managed_distribution {
        super::dto::ManagedEligibilityReason::RegistrationOnly
    } else if !installed {
        super::dto::ManagedEligibilityReason::NotInstalled
    } else {
        super::dto::ManagedEligibilityReason::Eligible
    };
    let next_action = if recovery_required {
        super::dto::ManagedNextAction::ResolveRecovery
    } else if interrupted {
        super::dto::ManagedNextAction::ResumeTask
    } else if in_progress {
        super::dto::ManagedNextAction::WaitForTask
    } else if record.managed.state.installation
        == crate::mcp_platform::InstallationState::RepairRequired
    {
        super::dto::ManagedNextAction::Repair
    } else if !record.managed.state.default_enabled {
        super::dto::ManagedNextAction::EnableAfterHealth
    } else {
        super::dto::ManagedNextAction::None
    };
    let lifecycle_busy = recovery_required || interrupted || in_progress;
    let active_version = record
        .lifecycle
        .active_version
        .as_deref()
        .and_then(|active| semver::Version::parse(active).ok());
    let eligible_update = available_manifests
        .as_ref()
        .and_then(|candidates| candidates.eligible.as_ref())
        .filter(|candidate| {
            active_version.as_ref().is_some_and(|active| {
                semver::Version::parse(&candidate.version)
                    .is_ok_and(|available| available > *active)
            })
        });
    let blocked_update = available_manifests
        .as_ref()
        .and_then(|candidates| candidates.blocked.as_ref())
        .filter(|candidate| {
            active_version.as_ref().is_some_and(|active| {
                semver::Version::parse(&candidate.version)
                    .is_ok_and(|available| available > *active)
            })
        });
    let selected_manifest = eligible_update.or(blocked_update).or_else(|| {
        available_manifests
            .as_ref()
            .and_then(|candidates| candidates.eligible.as_ref().or(candidates.blocked.as_ref()))
    });
    let update_available = eligible_update.is_some();
    let update_reason = if reason != super::dto::ManagedEligibilityReason::Eligible {
        reason
    } else if update_available {
        super::dto::ManagedEligibilityReason::Eligible
    } else if let Some(candidate) = blocked_update {
        candidate.reason
    } else {
        super::dto::ManagedEligibilityReason::NoUpdateAvailable
    };
    let available_version = selected_manifest.map(|candidate| candidate.version.clone());
    let available_manifest_digest = selected_manifest.map(|candidate| candidate.digest.clone());
    ManagedMcpSummary {
        managed_mcp_id: record.managed.managed_mcp_id,
        mcp_id: record.managed.mcp_id,
        installation_scope: record.managed.installation_scope,
        registration: record.managed.state.registration,
        installation: record.managed.state.installation,
        runtime: record.managed.state.runtime,
        health: record.managed.state.health,
        default_enabled: record.managed.state.default_enabled,
        revision: record.managed.revision,
        updated_at_ms: record.managed.updated_at_ms,
        distribution_adapter: record.lifecycle.distribution_adapter,
        active_version: record.lifecycle.active_version,
        available_version,
        available_manifest_digest,
        current_task,
        recovery_required,
        credential_status,
        external_capability,
        eligibility: super::dto::ManagedEligibility {
            update: !lifecycle_busy && managed_distribution && installed && update_available,
            repair: !lifecycle_busy && managed_distribution && installed,
            uninstall: !lifecycle_busy && managed_distribution && installed,
            reason,
            update_reason,
            repair_reason: reason,
            uninstall_reason: reason,
        },
        next_action,
    }
}

#[derive(Debug, Clone)]
struct AvailableManagedManifest {
    version: String,
    digest: String,
    reason: super::dto::ManagedEligibilityReason,
}

#[derive(Debug, Clone, Default)]
struct AvailableManagedManifests {
    eligible: Option<AvailableManagedManifest>,
    blocked: Option<AvailableManagedManifest>,
}

fn available_managed_manifests(
    records: Vec<crate::mcp_platform::repository::ManifestRecord>,
    target: CompatibilityTarget,
    runtime: RuntimeCapabilitySnapshot,
    external: ExternalCapabilitySnapshot,
    development_mode: bool,
) -> HashMap<(String, String), AvailableManagedManifests> {
    let mut candidates = HashMap::new();
    for record in records {
        let manifest = record.verified.manifest();
        let adapter = manifest.distribution.adapter_id().to_string();
        if !matches!(
            adapter.as_str(),
            "npm" | "python_wheel" | "binary_archive" | "docker" | "git_dev"
        ) {
            continue;
        }
        let Ok(version) = semver::Version::parse(manifest.version.as_str()) else {
            continue;
        };
        let key = (manifest.id.clone(), adapter);
        let context = PolicyContext::new(record.trust_tier, PlanOperation::Update)
            .with_target(target.platform, target.arch)
            .with_runtime_capabilities(runtime.node_available, runtime.python_major_minor)
            .with_external_capabilities(
                external.docker_available,
                external.git_available,
                development_mode,
            )
            .with_docker_policy(external.docker_policy_allowed);
        let candidate_result = if !manifest.host_integrations.is_empty()
            || compatibility(&record.verified, target) != CatalogCompatibility::Compatible
        {
            Err(super::dto::ManagedEligibilityReason::Incompatible)
        } else {
            plan_for_manifest(&record.verified, &context).map_err(|error| match error.code() {
                McpPlatformErrorCode::PolicyDenied => {
                    super::dto::ManagedEligibilityReason::PolicyDenied
                }
                McpPlatformErrorCode::DevelopmentModeRequired
                | McpPlatformErrorCode::GitOriginDenied
                | McpPlatformErrorCode::DaemonPolicyDenied
                | McpPlatformErrorCode::MountPermissionDenied => {
                    super::dto::ManagedEligibilityReason::PolicyDenied
                }
                McpPlatformErrorCode::AdapterIncompatible => {
                    super::dto::ManagedEligibilityReason::RuntimeUnavailable
                }
                McpPlatformErrorCode::DockerUnavailable | McpPlatformErrorCode::GitUnavailable => {
                    super::dto::ManagedEligibilityReason::RuntimeUnavailable
                }
                _ => super::dto::ManagedEligibilityReason::Incompatible,
            })
        };
        let candidate_set = candidates
            .entry(key)
            .or_insert_with(AvailableManagedManifests::default);
        let destination = if candidate_result.is_ok() {
            &mut candidate_set.eligible
        } else {
            &mut candidate_set.blocked
        };
        let replace = destination
            .as_ref()
            .and_then(|existing| semver::Version::parse(&existing.version).ok())
            .is_none_or(|existing| version > existing);
        if replace {
            *destination = Some(AvailableManagedManifest {
                version: manifest.version.as_str().to_string(),
                digest: record.verified.digest().to_string(),
                reason: candidate_result
                    .err()
                    .unwrap_or(super::dto::ManagedEligibilityReason::NoUpdateAvailable),
            });
        }
    }
    candidates
}

fn uninstall_plan(
    verified: &VerifiedManifest,
    trust_tier: crate::mcp_platform::TrustTier,
    managed_mcp_id: &str,
    preserve_user_data: bool,
    projection: crate::mcp_platform::ConnectionProjection,
    context: &PolicyContext,
) -> McpPlatformResult<InstallationPlan> {
    let manifest = verified.manifest();
    if !matches!(
        manifest.distribution,
        Distribution::Npm { .. }
            | Distribution::PythonWheel { .. }
            | Distribution::BinaryArchive { .. }
            | Distribution::Docker { .. }
            | Distribution::GitDev { .. }
    ) {
        return Err(operation_not_supported());
    }
    let policy = evaluate_manifest_policy(manifest, context);
    if policy.is_denied() {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::PolicyDenied,
            "managed uninstall was denied by policy",
        ));
    }
    InstallationPlan::new(
        manifest,
        verified.digest().to_string(),
        AdapterIdentity {
            id: manifest.distribution.adapter_id().to_string(),
            version: "1".to_string(),
        },
        trust_tier,
        PlanOperation::Uninstall,
        vec![PlanStep::RemoveManagedInstallation {
            managed_mcp_id: managed_mcp_id.to_string(),
            version: manifest.version.as_str().to_string(),
            preserve_user_data,
            ownership_only: true,
        }],
        EffectSummary {
            registers_connection: false,
            downloads_artifacts: false,
            writes_files: false,
            removes_files: true,
            requires_process_spawn: false,
            network_origins: Vec::new(),
            permission_ids: manifest
                .permissions
                .iter()
                .map(|permission| permission.id.clone())
                .collect(),
        },
        vec![PlanWarning::RemovesOwnedFilesOnly],
        Vec::new(),
        projection,
        policy,
    )
}

fn catalog_summary_from_entry(
    record: GovernedCatalogEntryRecord,
    target: CompatibilityTarget,
    eligibility: Eligibility,
) -> CatalogSummary {
    let manifest = record.manifest.verified.manifest();
    CatalogSummary {
        source_id: record.source.source_id,
        manifest_digest: record.manifest.verified.digest().to_string(),
        mcp_id: manifest.id.clone(),
        version: manifest.version.as_str().to_string(),
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        publisher_id: manifest.publisher.id.clone(),
        publisher_name: manifest.publisher.name.clone(),
        trust_tier: record.manifest.trust_tier,
        proof: record.manifest.proof,
        source_metadata: record.manifest.source_metadata,
        compatibility: compatibility(&record.manifest.verified, target),
        distribution_adapter: manifest.distribution.adapter_id().to_string(),
        verified_at_ms: record.manifest.created_at_ms,
        eligibility,
    }
}

fn source_context_for_metadata(
    source_metadata: &ManifestSourceMetadata,
) -> McpPlatformResult<ManifestSourceContext> {
    let source_context = match &source_metadata.source_ref {
        SourceRef::VerifiedSourceCatalog { .. } => ManifestSourceContext::GovernedCatalog {
            source_id: governed_source_id(&source_metadata.source_ref)?,
        },
        SourceRef::LocalPersistence => ManifestSourceContext::PersistedManifest {
            source_id: LOCAL_PERSISTED_SOURCE_ID.to_string(),
        },
        SourceRef::HttpsManifestUrl { manifest_url } => ManifestSourceContext::PersistedManifest {
            source_id: format!(
                "https_manifest_url_{}",
                crate::utils::bytes_to_hex(Sha256::digest(manifest_url.as_bytes()))
            ),
        },
        SourceRef::EnterpriseDirectory { .. } => {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::NotImplementedForPhase,
                "enterprise directory source context is not implemented for this phase",
            ));
        }
    };
    Ok(source_context)
}

fn source_id_for_metadata(source_metadata: &ManifestSourceMetadata) -> McpPlatformResult<String> {
    match &source_metadata.source_ref {
        SourceRef::LocalPersistence => Ok(LOCAL_PERSISTED_SOURCE_ID.to_string()),
        SourceRef::HttpsManifestUrl { manifest_url } => Ok(format!(
            "https_manifest_url_{}",
            crate::utils::bytes_to_hex(Sha256::digest(manifest_url.as_bytes()))
        )),
        SourceRef::VerifiedSourceCatalog { source_id } => Ok(source_id.clone()),
        SourceRef::EnterpriseDirectory { .. } => Err(McpPlatformError::new(
            McpPlatformErrorCode::NotImplementedForPhase,
            "enterprise directory source context is not implemented for this phase",
        )),
    }
}

fn trust_tier_for_source_ref(source_ref: &SourceRef) -> crate::mcp_platform::TrustTier {
    crate::mcp_platform::manifest::trust_tier_for_source_ref(source_ref)
        .unwrap_or(crate::mcp_platform::TrustTier::Local)
}

fn governed_source_record(
    source_ref: &SourceRef,
    display_name_override: Option<&str>,
    now_ms: i64,
) -> McpPlatformResult<GovernedCatalogSourceRecord> {
    Ok(GovernedCatalogSourceRecord {
        source_id: governed_source_id(source_ref)?,
        import_kind: source_ref.import_kind(),
        source_ref: source_ref.clone(),
        display_name: display_name_override
            .map(str::to_string)
            .unwrap_or_else(|| governed_source_display_name(source_ref)),
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    })
}

fn governed_manifest_document_record(
    source_id: &str,
    document_bytes: &[u8],
    now_ms: i64,
) -> GovernedCatalogDocumentRecord {
    let digest = sha256_hex(document_bytes);
    GovernedCatalogDocumentRecord {
        source_id: source_id.to_string(),
        document_id: format!("manifest_{digest}"),
        document_digest: digest,
        document_kind: GovernedCatalogDocumentKind::Manifest,
        created_at_ms: now_ms,
    }
}

fn governed_directory_document_record(
    source_id: &str,
    document_digest: &str,
    now_ms: i64,
) -> GovernedCatalogDocumentRecord {
    GovernedCatalogDocumentRecord {
        source_id: source_id.to_string(),
        document_id: format!("directory_{document_digest}"),
        document_digest: document_digest.to_string(),
        document_kind: GovernedCatalogDocumentKind::Directory,
        created_at_ms: now_ms,
    }
}

fn build_governed_catalog_entry(
    verified: &VerifiedManifest,
    source_metadata: ManifestSourceMetadata,
    now_ms: i64,
    release_id_override: Option<String>,
) -> McpPlatformResult<SaveGovernedCatalogEntry> {
    let trust_tier = trust_tier_for_source_ref(&source_metadata.source_ref);
    let proof = match &source_metadata.source_ref {
        SourceRef::VerifiedSourceCatalog { .. } => {
            let provenance = source_metadata
                .origin_provenance
                .verified_source_document
                .as_ref()
                .ok_or_else(invalid_request)?;
            ManifestProof::Catalog {
                index_digest: provenance.document_digest.clone(),
                declared_manifest_digest: verified.digest().to_string(),
                signature: None,
            }
        }
        SourceRef::LocalPersistence | SourceRef::HttpsManifestUrl { .. } => {
            ManifestProof::LocalBytes
        }
        SourceRef::EnterpriseDirectory { .. } => {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::NotImplementedForPhase,
                "enterprise directory import is not implemented for this phase",
            ));
        }
    };
    Ok(SaveGovernedCatalogEntry {
        entry_id: release_id_override.unwrap_or_else(|| verified.digest().to_string()),
        manifest: ManifestRecord {
            verified: verified.clone(),
            proof,
            trust_tier,
            source_metadata,
            created_at_ms: now_ms,
        },
    })
}

fn validate_supported_governed_source_metadata(
    source_metadata: &ManifestSourceMetadata,
) -> McpPlatformResult<()> {
    source_metadata.validate()?;
    if matches!(
        source_metadata.source_ref,
        SourceRef::EnterpriseDirectory { .. }
    ) {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::NotImplementedForPhase,
            "enterprise directory import is not implemented for this phase",
        ));
    }
    Ok(())
}

fn validate_verified_catalog_directory_metadata(
    source_metadata: &ManifestSourceMetadata,
) -> McpPlatformResult<()> {
    validate_supported_governed_source_metadata(source_metadata)?;
    if !matches!(
        source_metadata.source_ref,
        SourceRef::VerifiedSourceCatalog { .. }
    ) || source_metadata.release_id.is_some()
    {
        return Err(invalid_request());
    }
    Ok(())
}

fn validate_verified_directory_provenance(
    source_metadata: &ManifestSourceMetadata,
    document: &crate::verified_source_catalog::VerifiedSourceDocument,
) -> McpPlatformResult<()> {
    let Some(provenance) = source_metadata
        .origin_provenance
        .verified_source_document
        .as_ref()
    else {
        return Err(invalid_request());
    };
    if provenance.source_id != document.envelope.payload.source_id
        || provenance.document_digest != document.digests.document_digest
        || provenance.canonical_digest != document.digests.canonical_digest
        || provenance.signed_digest != document.digests.signed_digest
        || provenance.binding_digest != document.digests.binding_digest
    {
        return Err(invalid_request());
    }
    let actual_kids = document
        .envelope
        .signatures
        .iter()
        .map(|signature| signature.kid.clone())
        .collect::<Vec<_>>();
    if provenance.signature_kids != actual_kids {
        return Err(invalid_request());
    }
    Ok(())
}

fn verified_source_document_contains_release(
    document: &crate::verified_source_catalog::VerifiedSourceDocument,
    release_id: &str,
    mcp_id: &str,
    version: &str,
) -> bool {
    document
        .envelope
        .payload
        .snapshot
        .releases
        .iter()
        .any(|release| {
            release.release_id == release_id
                && release.mcp_id == mcp_id
                && release.version == version
        })
}

fn validate_verified_provenance_against_stored(
    source_metadata: &ManifestSourceMetadata,
    stored: &crate::mcp_platform::repository::sqlite::StoredSourceCatalogDocumentProvenance,
) -> McpPlatformResult<()> {
    let Some(provenance) = source_metadata
        .origin_provenance
        .verified_source_document
        .as_ref()
    else {
        return Err(invalid_request());
    };
    if provenance.document_digest != stored.document_digest
        || provenance.canonical_digest != stored.canonical_digest
        || provenance.signed_digest != stored.signed_digest
        || provenance.binding_digest != stored.binding_digest
        || provenance.signature_kids != stored.signature_kids
    {
        return Err(invalid_request());
    }
    Ok(())
}

fn ensure_import_eligible(eligibility: Eligibility) -> McpPlatformResult<()> {
    if eligibility.outcome == EligibilityOutcome::Denied {
        Err(McpPlatformError::new(
            McpPlatformErrorCode::PolicyDenied,
            "manifest is not eligible for governed catalog import",
        ))
    } else {
        Ok(())
    }
}

fn governed_source_id(source_ref: &SourceRef) -> McpPlatformResult<String> {
    match source_ref {
        SourceRef::LocalPersistence => Ok(LOCAL_PERSISTED_SOURCE_ID.to_string()),
        SourceRef::HttpsManifestUrl { manifest_url } => Ok(format!(
            "https_manifest_url_{}",
            sha256_hex(manifest_url.as_bytes())
        )),
        SourceRef::VerifiedSourceCatalog { source_id } => {
            Ok(format!("verified_source_catalog_{source_id}"))
        }
        SourceRef::EnterpriseDirectory {
            directory_id,
            entry_id,
        } => {
            let mut canonical = Vec::with_capacity(8 + directory_id.len() + entry_id.len());
            canonical.extend_from_slice(b"enterprise\0");
            canonical.extend_from_slice(&(directory_id.len() as u64).to_be_bytes());
            canonical.extend_from_slice(directory_id.as_bytes());
            canonical.extend_from_slice(&(entry_id.len() as u64).to_be_bytes());
            canonical.extend_from_slice(entry_id.as_bytes());
            Ok(format!("enterprise_directory_{}", sha256_hex(&canonical)))
        }
    }
}

fn governed_source_display_name(source_ref: &SourceRef) -> String {
    match source_ref {
        SourceRef::LocalPersistence => "Local persistence".to_string(),
        SourceRef::HttpsManifestUrl { manifest_url } => Url::parse(manifest_url)
            .ok()
            .and_then(|url| {
                url.host_str()
                    .map(|host| format!("HTTPS manifest URL ({host})"))
            })
            .unwrap_or_else(|| "HTTPS manifest URL".to_string()),
        SourceRef::VerifiedSourceCatalog { source_id } => {
            format!("Verified source catalog ({source_id})")
        }
        SourceRef::EnterpriseDirectory { directory_id, .. } => {
            format!("Enterprise directory ({directory_id})")
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    crate::utils::bytes_to_hex(Sha256::digest(bytes))
}

fn https_manifest_preview(
    fetched: &HttpsManifestFetchResult,
    verified: &VerifiedManifest,
) -> HttpsManifestPreview {
    HttpsManifestPreview {
        manifest_id: verified.manifest().id.clone(),
        version: verified.manifest().version.as_str().to_string(),
        redacted_origin: fetched.final_url.redacted(),
        raw_digest: sha256_hex(&fetched.raw_bytes),
        parsed_digest: verified.digest().to_string(),
        redirect_chain_digest: sha256_hex(&fetched.redirect_chain_digest),
        dns_evidence_digest: sha256_hex(&fetched.dns_evidence_digest),
    }
}

#[cfg(test)]
fn validate_intake_raw_reference(value: &str) -> McpPlatformResult<()> {
    if value.is_empty()
        || value.len() > 256
        || value
            .bytes()
            .any(|byte| matches!(byte, b':' | b'/' | b'\\' | b'?'))
        || value.to_ascii_lowercase().contains("bearer")
        || value.to_ascii_lowercase().contains("token")
    {
        return Err(invalid_request());
    }
    Ok(())
}

fn source_provision_preview_from_stage(
    preview: &crate::mcp_platform::source_provisioning::SourceProvisioningPreview,
) -> SourceProvisionPreview {
    SourceProvisionPreview {
        source_id: preview.source_id.clone(),
        display_name: preview.display_name.clone(),
        root_digest: preview.root_digest.clone(),
        document_digest: preview.document_digest.clone(),
        endpoint_host: preview.endpoint_host.clone(),
        refresh_transport: match preview.transport_kind {
            crate::mcp_platform::repository::GovernedSourceRefreshTransportKind::VerifiedSourceBundleV1 => {
                SourceProvisionRefreshTransport::VerifiedSourceBundleV1
            }
        },
        manifest_count: preview.manifest_count,
        warnings: preview.warnings.clone(),
        trust_basis: match preview.trust_basis {
            crate::mcp_platform::repository::GovernedSourceTrustBasis::UserPin => {
                SourceProvisionTrustBasis::UserPin
            }
        },
    }
}

const fn source_provisioning_transport_required() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::PolicyDenied,
        "source provisioning requires an authenticated transport session",
    )
}

fn manifest_key(record: &crate::mcp_platform::repository::ManifestRecord) -> (&str, &str, &str) {
    (
        &record.verified.manifest().id,
        record.verified.manifest().version.as_str(),
        record.verified.digest(),
    )
}

fn compatibility(manifest: &VerifiedManifest, target: CompatibilityTarget) -> CatalogCompatibility {
    match &manifest.manifest().distribution {
        Distribution::ManualStdio { platforms, .. } => {
            if platforms.is_empty()
                || platforms.contains(&Platform::Any)
                || platforms.contains(&target.platform)
            {
                CatalogCompatibility::Compatible
            } else {
                CatalogCompatibility::Incompatible
            }
        }
        Distribution::Npm { artifacts, .. }
        | Distribution::PythonWheel { artifacts, .. }
        | Distribution::BinaryArchive { artifacts, .. } => {
            if artifacts.iter().any(|artifact| {
                (artifact.platform == Platform::Any || artifact.platform == target.platform)
                    && (artifact.arch == Architecture::Any || artifact.arch == target.arch)
            }) {
                CatalogCompatibility::Compatible
            } else {
                CatalogCompatibility::Incompatible
            }
        }
        Distribution::RemoteHttp | Distribution::Docker { .. } | Distribution::GitDev { .. } => {
            CatalogCompatibility::Compatible
        }
    }
}

fn eligibility_from_error(code: McpPlatformErrorCode) -> Eligibility {
    let (reason, recovery) = match code {
        McpPlatformErrorCode::DevelopmentModeRequired => (
            EligibilityReason::DevelopmentModeRequired,
            RecoverySuggestion::EnableDevelopmentMode,
        ),
        McpPlatformErrorCode::AdapterIncompatible => (
            EligibilityReason::RuntimeUnavailable,
            RecoverySuggestion::InstallRequiredRuntime,
        ),
        McpPlatformErrorCode::DockerUnavailable | McpPlatformErrorCode::GitUnavailable => (
            EligibilityReason::ExternalCapabilityUnavailable,
            RecoverySuggestion::InstallRequiredRuntime,
        ),
        McpPlatformErrorCode::RemoteHttpPolicyUnavailable => (
            EligibilityReason::ExternalCapabilityUnavailable,
            RecoverySuggestion::ContactPolicyAdministrator,
        ),
        _ => (
            EligibilityReason::PolicyDenied,
            RecoverySuggestion::ContactPolicyAdministrator,
        ),
    };
    Eligibility {
        outcome: EligibilityOutcome::Denied,
        reason,
        recovery,
    }
}

fn manual_connection_id(idempotency_key: &str) -> String {
    let suffix = Sha256::digest(idempotency_key.as_bytes())
        .iter()
        .take(12)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("manual-http-{suffix}")
}

fn manual_http_manifest(
    connection_id: &str,
    endpoint: &str,
) -> McpPlatformResult<VerifiedManifest> {
    let url = Url::parse(endpoint).map_err(|_| invalid_request())?;
    let host = url.host_str().ok_or_else(invalid_request)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::IpAddr>().is_ok()
        || !host.contains('.')
    {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::UnsafeUrl,
            "manual HTTP endpoints must use credential-free public HTTPS origins",
        ));
    }
    let permissions = vec![serde_json::json!({
        "id":"remote-network",
        "kind":"network",
        "reason":"Connects to the reviewed remote MCP HTTPS origin.",
        "required":true,
        "scope":url.origin().ascii_serialization()
    })];
    let raw = serde_json::json!({
        "schema_version":1,
        "id":connection_id,
        "version":"0.0.0",
        "name":format!("Manual MCP at {host}"),
        "description":"A manually registered remote MCP connection validated by goose Core.",
        "publisher":{"id":"manual-local","name":"Local manual connection"},
        "license":{"spdx":"LicenseRef-Manual"},
        "capabilities":["tools"],
        "permissions":permissions,
        "distribution":{"type":"remote_http"},
        "transport":{"type":"streamable_http","url":endpoint},
        "auth":{"type":"none"},
        "health_check":{"type":"mcp_initialize","timeout_seconds":30},
        "owned_files":[],
        "uninstall":{"mode":"remove_owned_files_only","preserve_user_data":true}
    });
    serde_json::to_vec(&raw)
        .map_err(|_| invalid_request())
        .and_then(|bytes| parse_manifest(&bytes))
}

fn platform_name(platform: Platform) -> String {
    match platform {
        Platform::Windows => "windows",
        Platform::Macos => "macos",
        Platform::Linux => "linux",
        Platform::Any => "any",
    }
    .to_string()
}

fn architecture_name(architecture: Architecture) -> String {
    match architecture {
        Architecture::X86_64 => "x86_64",
        Architecture::Aarch64 => "aarch64",
        Architecture::Universal => "universal",
        Architecture::Any => "any",
    }
    .to_string()
}

fn managed_credential_status(auth: &Auth) -> Option<CredentialStatus> {
    match auth {
        Auth::None => Some(CredentialStatus::Ready),
        _ => Some(CredentialStatus::TemporarilyUnavailable),
    }
}

fn ensure_plan_envelope_matches(
    stored: &PlanRecord,
    expected_plan: &InstallationPlan,
    expected_target: &PlanTarget,
) -> McpPlatformResult<()> {
    if stored.plan.plan_digest() == expected_plan.plan_digest() && stored.target == *expected_target
    {
        Ok(())
    } else {
        Err(idempotency_conflict())
    }
}

fn validate_page_size(value: Option<u16>) -> McpPlatformResult<u16> {
    let value = value.unwrap_or(DEFAULT_PAGE_SIZE);
    if (1..=MAX_PAGE_SIZE).contains(&value) {
        Ok(value)
    } else {
        Err(invalid_request())
    }
}

fn validate_event_limit(value: Option<u16>) -> McpPlatformResult<u16> {
    let value = value.unwrap_or(DEFAULT_EVENT_LIMIT);
    if (1..=MAX_EVENT_LIMIT).contains(&value) {
        Ok(value)
    } else {
        Err(invalid_request())
    }
}

fn validate_optional_text(value: Option<&str>, max: usize) -> McpPlatformResult<()> {
    if value.is_some_and(|value| value.len() > max) {
        Err(invalid_request())
    } else {
        Ok(())
    }
}

fn build_governed_source_refresh_input(
    frozen: &FrozenSourceProvisioningSnapshot,
    parsed: &crate::mcp_platform::source_provisioning::ParsedSourceProvisioningSnapshot,
    source_id: &str,
) -> McpPlatformResult<GovernedDirectoryImportInput> {
    if parsed.descriptor.source_id != source_id {
        return Err(invalid_request());
    }
    let document = &parsed.source_document;
    let provenance = VerifiedSourceDocumentRef {
        source_id: source_id.to_string(),
        document_digest: document.digests.document_digest.clone(),
        canonical_digest: document.digests.canonical_digest.clone(),
        signed_digest: document.digests.signed_digest.clone(),
        binding_digest: document.digests.binding_digest.clone(),
        signature_kids: document
            .envelope
            .signatures
            .iter()
            .map(|signature| signature.kid.clone())
            .collect(),
    };
    Ok(GovernedDirectoryImportInput {
        directory_document_bytes: frozen.source_document_bytes.clone(),
        source_metadata: ManifestSourceMetadata {
            source_ref: SourceRef::VerifiedSourceCatalog {
                source_id: source_id.to_string(),
            },
            import_kind: SourceImportKind::VerifiedSourceCatalog,
            release_id: None,
            origin_provenance: OriginProvenance {
                verified_source_document: Some(provenance),
            },
            update_channel: Default::default(),
        },
        manifests: frozen
            .manifests
            .iter()
            .map(|manifest| GovernedDirectoryManifestImportInput {
                release_id: manifest.release_id.clone(),
                document_bytes: manifest.document_bytes.clone(),
            })
            .collect(),
    })
}

fn refresh_not_registered() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::NotFound,
        "governed source refresh is not registered",
    )
}

fn validate_identifier(value: &str) -> McpPlatformResult<()> {
    if value.is_empty() || value.len() > MAX_ID_LENGTH || value.chars().any(char::is_control) {
        Err(invalid_request())
    } else {
        Ok(())
    }
}

fn validate_idempotency_key(value: &str) -> McpPlatformResult<()> {
    validate_identifier(value)
}

fn validate_digest(value: &str) -> McpPlatformResult<()> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(invalid_request())
    }
}

fn validate_revision(value: i64) -> McpPlatformResult<()> {
    if value >= 0 {
        Ok(())
    } else {
        Err(invalid_request())
    }
}

const fn current_platform() -> Platform {
    if cfg!(target_os = "windows") {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::Macos
    } else {
        Platform::Linux
    }
}

const fn current_architecture() -> Architecture {
    if cfg!(target_arch = "aarch64") {
        Architecture::Aarch64
    } else {
        Architecture::X86_64
    }
}

const fn invalid_request() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::InvalidRequest,
        "MCP platform request is invalid",
    )
}

fn canonical_https_url(value: &str) -> McpPlatformResult<String> {
    let url = ValidatedHttpsUrl::parse(value).map_err(|_| invalid_request())?;
    Ok(url.request_url().as_str().to_string())
}

fn canonical_https_url_for_record(value: &str) -> Option<String> {
    canonical_https_url(value).ok()
}

const fn manifest_external_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::InvalidRequest,
        "HTTPS manifest operation could not be completed",
    )
}

const fn manifest_rejection_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "HTTPS manifest operation was safely rejected",
    )
}

const fn integrity_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "stored MCP platform data failed integrity validation",
    )
}

const fn not_found() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::NotFound,
        "MCP platform record was not found",
    )
}

const fn operation_not_supported() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::OperationNotSupported,
        "operation is not supported in phase 2C",
    )
}

const fn plan_stale() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::PlanStale,
        "installation plan digest no longer matches",
    )
}

const fn plan_expired() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::PlanExpired,
        "installation plan has expired",
    )
}

const fn idempotency_conflict() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IdempotencyConflict,
        "idempotency key already refers to different content",
    )
}

const fn invalid_transition() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::InvalidTransition,
        "task state does not permit this operation",
    )
}

const fn revision_conflict() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RevisionConflict,
        "record revision no longer matches",
    )
}

const fn credential_missing() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::CredentialMissing,
        "managed remote HTTP credentials are unavailable",
    )
}

fn lifecycle_ports(remote_http: Arc<dyn RemoteHttpNetworkPolicy>) -> LifecyclePorts {
    LifecyclePorts {
        registration: Arc::new(SafeRegistrationEffectAdapter::new(remote_http.clone())),
        host_integration: Arc::new(EmptyHostIntegrationAdapter),
        transport: Arc::new(CoreTransportProjectionAdapter),
        auth: Arc::new(ConfigAuthRequirementResolver),
        health: Arc::new(ProductionHealthCheckAdapter::new(remote_http)),
        projection_sink: Arc::new(ConfigProjectionSink::default()),
    }
}

impl Drop for McpPlatformService {
    fn drop(&mut self) {
        self.runner.shutdown();
        if let Ok(WorkerState::Running(handle)) = self.worker.get_mut() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REMOTE: &str =
        include_str!("../../../../../documentation/static/schemas/examples/remote-http.json");
    const NPM: &str =
        include_str!("../../../../../documentation/static/schemas/examples/npm-package.json");

    #[test]
    fn plan_review_reversibility_is_operation_specific() {
        let remote = parse_manifest(REMOTE.as_bytes()).unwrap();
        let register = plan_for_manifest(
            &remote,
            &PolicyContext::new(
                crate::mcp_platform::TrustTier::Local,
                PlanOperation::Register,
            ),
        )
        .unwrap();
        assert_eq!(
            plan_reversibility(&register),
            PlanReversibility {
                reversible: true,
                strategy: RollbackStrategy::RemoveConnectionRegistration,
            }
        );

        let npm = parse_manifest(NPM.as_bytes()).unwrap();
        let install_context = PolicyContext::new(
            crate::mcp_platform::TrustTier::Local,
            PlanOperation::Install,
        )
        .with_runtime_capabilities(true, Some((3, 11)));
        let install = plan_for_manifest(&npm, &install_context).unwrap();
        assert_eq!(
            plan_reversibility(&install),
            PlanReversibility {
                reversible: true,
                strategy: RollbackStrategy::StagedActivationRestoresPreviousVersion,
            }
        );

        let uninstall_context = PolicyContext::new(
            crate::mcp_platform::TrustTier::Local,
            PlanOperation::Uninstall,
        );
        let uninstall = uninstall_plan(
            &npm,
            crate::mcp_platform::TrustTier::Local,
            "managed_test",
            true,
            install.connection_projection().clone(),
            &uninstall_context,
        )
        .unwrap();
        assert_eq!(
            plan_reversibility(&uninstall),
            PlanReversibility {
                reversible: false,
                strategy: RollbackStrategy::UninstallRemovesOwnedFiles {
                    preserve_user_data: true,
                },
            }
        );
    }

    #[test]
    fn managed_summary_projects_runtime_credential_status() {
        let record = ManagedMcpInventoryRecord {
            managed: crate::mcp_platform::repository::ManagedMcpRecord {
                managed_mcp_id: "managed_test".to_string(),
                mcp_id: "remote-test".to_string(),
                installation_scope: "user".to_string(),
                state: ManagedMcpState::default(),
                revision: 1,
                versions: Vec::new(),
                created_at_ms: 0,
                updated_at_ms: 0,
            },
            lifecycle: crate::mcp_platform::repository::ManagedMcpLifecycleMetadata {
                distribution_adapter: "remote_http".to_string(),
                active_manifest_digest: Some("manifest".to_string()),
                active_version: Some("1.0.0".to_string()),
                owner_task_id: None,
            },
        };
        let summary = managed_summary(
            record.clone(),
            None,
            false,
            None,
            ExternalCapabilitySnapshot::default(),
            managed_credential_status(&Auth::None),
        );
        assert_eq!(summary.credential_status, Some(CredentialStatus::Ready));

        let unavailable = managed_credential_status(&Auth::ApiKeyHeader {
            header_name: "x-api-key".to_string(),
            prefix: None,
            credential_name: "test".to_string(),
        });
        assert_eq!(unavailable, Some(CredentialStatus::TemporarilyUnavailable));
        let missing = managed_summary(
            record,
            None,
            false,
            None,
            ExternalCapabilitySnapshot::default(),
            None,
        );
        assert_eq!(missing.credential_status, None);
    }

    #[test]
    fn source_policy_ids_keep_enterprise_and_other_sources_separate() {
        let enterprise = SourceRef::EnterpriseDirectory {
            directory_id: "corp".to_string(),
            entry_id: "entry".to_string(),
        };
        let https = SourceRef::HttpsManifestUrl {
            manifest_url: "https://example.com/manifest.json".to_string(),
        };

        assert!(governed_source_id(&enterprise)
            .unwrap()
            .starts_with("enterprise_directory_"));
        assert_ne!(
            governed_source_id(&enterprise).unwrap(),
            governed_source_id(&https).unwrap()
        );
        assert_eq!(
            governed_source_display_name(&enterprise),
            "Enterprise directory (corp)"
        );
    }

    #[test]
    fn enterprise_source_ids_are_structured_and_collision_resistant() {
        let first = SourceRef::EnterpriseDirectory {
            directory_id: "a_b".to_string(),
            entry_id: "c".to_string(),
        };
        let second = SourceRef::EnterpriseDirectory {
            directory_id: "a".to_string(),
            entry_id: "b_c".to_string(),
        };
        assert_ne!(
            governed_source_id(&first).unwrap(),
            governed_source_id(&second).unwrap()
        );
        assert_eq!(
            governed_source_id(&first).unwrap(),
            governed_source_id(&first).unwrap()
        );

        let ascii = SourceRef::EnterpriseDirectory {
            directory_id: "Corp-Directory".to_string(),
            entry_id: "Entry-A".to_string(),
        };
        let case_variant = SourceRef::EnterpriseDirectory {
            directory_id: "corp-directory".to_string(),
            entry_id: "entry-a".to_string(),
        };
        let long = SourceRef::EnterpriseDirectory {
            directory_id: "x".repeat(1024),
            entry_id: "y".repeat(1024),
        };
        let ascii_id = governed_source_id(&ascii).unwrap();
        let case_id = governed_source_id(&case_variant).unwrap();
        let long_id = governed_source_id(&long).unwrap();
        assert_ne!(ascii_id, case_id);
        assert_ne!(ascii_id, long_id);
        assert_eq!(ascii_id.len(), "enterprise_directory_".len() + 64);
        assert_eq!(long_id.len(), "enterprise_directory_".len() + 64);
        assert!(!ascii_id.contains("Corp"));
        assert!(!case_id.contains("corp"));
        assert!(!long_id.contains(&"x".repeat(32)));
    }

    #[test]
    fn https_manifest_canonical_url_preserves_safe_query() {
        let canonical = canonical_https_url("https://example.com/manifest.json?version=1").unwrap();
        assert_eq!(canonical, "https://example.com/manifest.json?version=1");
    }

    #[test]
    fn https_manifest_canonical_url_rejects_unsafe_boundaries() {
        assert!(canonical_https_url("https://example.com/manifest.json#private").is_err());
        assert!(canonical_https_url("https://user:password@example.com/manifest.json").is_err());
        assert!(canonical_https_url("https://localhost/manifest.json").is_err());
        assert!(canonical_https_url("https://service.localhost/manifest.json").is_err());
        assert!(canonical_https_url("https://local/manifest.json").is_err());
        assert!(canonical_https_url("https://service.local/manifest.json").is_err());
        assert!(canonical_https_url("http://example.com/manifest.json").is_err());
    }

    #[test]
    fn https_manifest_preview_and_debug_are_redacted() {
        let fetched = HttpsManifestFetchResult {
            requested: ValidatedHttpsUrl::parse("https://example.com/manifest.json?token=secret")
                .unwrap(),
            final_url: ValidatedHttpsUrl::parse("https://example.com/manifest.json?token=secret")
                .unwrap(),
            redirect_chain: vec![ValidatedHttpsUrl::parse(
                "https://example.com/manifest.json?token=secret",
            )
            .unwrap()],
            raw_bytes: REMOTE.as_bytes().to_vec(),
            raw_sha256: Sha256::digest(REMOTE.as_bytes()).into(),
            content_type: "application/json".to_string(),
            redirect_chain_digest: [1; 32],
            dns_evidence_digest: [4; 32],
        };
        let verified = parse_manifest(&fetched.raw_bytes).unwrap();
        let preview = https_manifest_preview(&fetched, &verified);
        let debug = format!("{preview:?}");
        assert!(!debug.contains("token=secret"));
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("REMOTE"));
        assert!(!debug.contains("127.0.0.1"));
        assert_eq!(preview.redacted_origin, "https://example.com");
        assert_eq!(preview.raw_digest, sha256_hex(&fetched.raw_bytes));
        assert_eq!(preview.redirect_chain_digest, sha256_hex(&[1; 32]));
        assert_eq!(preview.dns_evidence_digest, sha256_hex(&[4; 32]));
    }

    #[test]
    fn https_manifest_result_debug_hides_token_and_preview_contents() {
        let result = HttpsManifestPrepareResult {
            provision_id: "provision".to_string(),
            server_token: "https_manifest_confirmation_secret".to_string(),
            expires_at_ms: 42,
            preview: HttpsManifestPreview {
                manifest_id: "manifest".to_string(),
                version: "1.0.0".to_string(),
                redacted_origin: "https://example.com".to_string(),
                raw_digest: "a".repeat(64),
                parsed_digest: "b".repeat(64),
                redirect_chain_digest: "c".repeat(64),
                dns_evidence_digest: "d".repeat(64),
            },
        };
        let debug = format!("{result:?}");
        assert!(!debug.contains("https_manifest_confirmation_secret"));
        assert!(!debug.contains("secret"));
        assert!(debug.contains("server_token_present: true"));
        assert!(!format!("{:?}", manifest_external_error()).contains("secret"));
        assert!(!format!("{:?}", manifest_rejection_error()).contains("secret"));

        let confirm = HttpsManifestConfirmResult {
            provision_id: "confirm-provision".to_string(),
            confirmed_at_ms: 43,
            manifest_digest: "e".repeat(64),
            preview: result.preview,
        };
        assert!(!confirm.manifest_digest.is_empty());
        let confirm_debug = format!("{confirm:?}");
        assert!(!confirm_debug.contains("https://example.com/manifest.json"));
        assert!(!confirm_debug.contains("token=secret"));
        assert!(!confirm_debug.contains("https_manifest_confirmation_secret"));
        assert!(!confirm_debug.contains("REMOTE"));
        assert!(!confirm_debug.contains("127.0.0.1"));
    }
}
