use super::*;
use crate::mcp_platform::manifest::{
    Architecture, ArchiveFormat, Auth, Capability, Distribution, GitDevAdapter, HealthCheck,
    Manifest, ManifestProof, OAuthClientRegistration, PermissionKind, Platform, Transport,
};
use crate::mcp_platform::policy::PolicyOutcome;
use crate::mcp_platform::service::{
    CatalogListInput, CatalogLocator, EventsResumeInput, HealthGetInput, HealthRunInput,
    InstallConfirmInput, InstallationScope, ManagedGetInput, ManagedListInput, PlanCreateInput,
    PlanIntent, SetDefaultEnabledInput, TaskCancelInput, TaskGetInput, TaskRetryInput,
    UserDecision,
};
use crate::mcp_platform::task::{RecoveryDecision, TaskOperation, TaskStatus, TaskStepStatus};
use crate::mcp_platform::{AuditEventType, AuditPayload};

impl GooseAcpAgent {
    pub(super) async fn on_mcp_catalog_list(
        &self,
        req: McpCatalogListRequest,
    ) -> McpCatalogListResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .catalog_list(
                    &context,
                    CatalogListInput {
                        query: req.query,
                        trust_tiers: req.trust_tiers.into_iter().map(trust_from_wire).collect(),
                        source_ids: req.source_ids,
                        compatibility: req.compatibility.map(compatibility_from_wire),
                        cursor: req.cursor,
                        page_size: req.page_size,
                    },
                )
                .await
                .map(catalog_page_to_wire),
            Err(error) => Err(error),
        };
        McpCatalogListResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_catalog_detail(
        &self,
        req: McpCatalogDetailRequest,
    ) -> McpCatalogDetailResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let locator = match req.catalog {
            McpCatalogLocator::ManifestDigest { manifest_digest } => {
                CatalogLocator::ManifestDigest(manifest_digest)
            }
            McpCatalogLocator::CatalogRef {
                source_id,
                mcp_id,
                version,
            } => CatalogLocator::CatalogRef {
                source_id,
                mcp_id,
                version,
            },
        };
        let result = match service {
            Ok(service) => service
                .catalog_detail(&context, locator)
                .await
                .map(catalog_detail_to_wire),
            Err(error) => Err(error),
        };
        McpCatalogDetailResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_plan_create(
        &self,
        req: McpPlanCreateRequest,
    ) -> McpPlanCreateResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let intent = match req.intent {
            McpPlanIntent::Register {
                manifest_digest,
                installation_scope,
            } => PlanIntent::Register {
                manifest_digest,
                installation_scope: match installation_scope.unwrap_or_default() {
                    McpInstallationScope::User => InstallationScope::User,
                },
            },
            McpPlanIntent::Install { manifest_digest } => PlanIntent::Install { manifest_digest },
            McpPlanIntent::Update {
                managed_mcp_id,
                target_version,
            } => PlanIntent::Update {
                managed_mcp_id,
                target_version,
            },
            McpPlanIntent::Repair { managed_mcp_id } => PlanIntent::Repair { managed_mcp_id },
            McpPlanIntent::Uninstall {
                managed_mcp_id,
                preserve_user_data,
            } => PlanIntent::Uninstall {
                managed_mcp_id,
                preserve_user_data,
            },
        };
        let result = match service {
            Ok(service) => service
                .plan_create(
                    &context,
                    PlanCreateInput {
                        intent,
                        idempotency_key: req.idempotency_key,
                    },
                )
                .await
                .map(plan_review_to_wire),
            Err(error) => Err(error),
        };
        McpPlanCreateResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_install_confirm(
        &self,
        req: McpInstallConfirmRequest,
    ) -> McpInstallConfirmResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .install_confirm(
                    &context,
                    InstallConfirmInput {
                        plan_id: req.plan_id,
                        plan_digest: req.plan_digest,
                        decision: match req.user_decision {
                            McpUserDecision::Confirm => UserDecision::Confirm,
                            McpUserDecision::Reject => UserDecision::Reject,
                        },
                        idempotency_key: req.idempotency_key,
                    },
                )
                .await
                .map(task_to_wire),
            Err(error) => Err(error),
        };
        McpInstallConfirmResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_task_get(&self, req: McpTaskGetRequest) -> McpTaskGetResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .task_get(
                    &context,
                    TaskGetInput {
                        task_id: req.task_id,
                    },
                )
                .await
                .map(task_to_wire),
            Err(error) => Err(error),
        };
        McpTaskGetResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_task_cancel(
        &self,
        req: McpTaskCancelRequest,
    ) -> McpTaskCancelResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .task_cancel(
                    &context,
                    TaskCancelInput {
                        task_id: req.task_id,
                        expected_revision: req.expected_revision,
                    },
                )
                .await
                .map(task_to_wire),
            Err(error) => Err(error),
        };
        McpTaskCancelResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_task_retry(&self, req: McpTaskRetryRequest) -> McpTaskRetryResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .task_retry(
                    &context,
                    TaskRetryInput {
                        task_id: req.task_id,
                        expected_revision: req.expected_revision,
                        idempotency_key: req.idempotency_key,
                    },
                )
                .await
                .map(task_to_wire),
            Err(error) => Err(error),
        };
        McpTaskRetryResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_events_resume(
        &self,
        req: McpEventsResumeRequest,
    ) -> McpEventsResumeResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .events_resume(
                    &context,
                    EventsResumeInput {
                        after_event_id: req.after_event_id,
                        limit: req.limit,
                        task_ids: req.task_ids,
                    },
                )
                .await
                .map(events_to_wire),
            Err(error) => Err(error),
        };
        McpEventsResumeResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_list(&self, req: McpListRequest) -> McpListResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .managed_list(
                    &context,
                    ManagedListInput {
                        cursor: req.cursor,
                        page_size: req.page_size,
                        registration: req.registration.map(registration_from_wire),
                        installation: req.installation.map(installation_from_wire),
                        runtime: req.runtime.map(runtime_from_wire),
                        health: req.health.map(health_from_wire),
                        default_enabled: req.default_enabled,
                    },
                )
                .await
                .map(managed_page_to_wire),
            Err(error) => Err(error),
        };
        McpListResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_get(&self, req: McpGetRequest) -> McpGetResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .managed_get(
                    &context,
                    ManagedGetInput {
                        managed_mcp_id: req.managed_mcp_id,
                    },
                )
                .await
                .map(managed_detail_to_wire),
            Err(error) => Err(error),
        };
        McpGetResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_health_run(&self, req: McpHealthRunRequest) -> McpHealthRunResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .health_run(
                    &context,
                    HealthRunInput {
                        managed_mcp_id: req.managed_mcp_id,
                        mode: match req.mode {
                            McpHealthCheckMode::Registration => {
                                crate::mcp_platform::HealthCheckMode::Registration
                            }
                            McpHealthCheckMode::Runtime => {
                                crate::mcp_platform::HealthCheckMode::Runtime
                            }
                        },
                        idempotency_key: req.idempotency_key,
                    },
                )
                .await
                .map(task_to_wire),
            Err(error) => Err(error),
        };
        McpHealthRunResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_health_get(&self, req: McpHealthGetRequest) -> McpHealthGetResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .health_get(
                    &context,
                    HealthGetInput {
                        managed_mcp_id: req.managed_mcp_id,
                    },
                )
                .await
                .map(health_status_to_wire),
            Err(error) => Err(error),
        };
        McpHealthGetResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_set_default_enabled(
        &self,
        req: McpSetDefaultEnabledRequest,
    ) -> McpSetDefaultEnabledResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .set_default_enabled(
                    &context,
                    SetDefaultEnabledInput {
                        managed_mcp_id: req.managed_mcp_id,
                        enabled: req.enabled,
                        expected_revision: req.expected_revision,
                    },
                )
                .await
                .map(managed_summary_to_wire),
            Err(error) => Err(error),
        };
        McpSetDefaultEnabledResponse {
            outcome: outcome(&context, result),
        }
    }
}

fn outcome<T>(
    context: &RequestContext,
    result: crate::mcp_platform::McpPlatformResult<T>,
) -> McpPlatformOutcome<T> {
    match result {
        Ok(value) => McpPlatformOutcome::success(value),
        Err(error) => McpPlatformOutcome::error(error_to_wire(context, error)),
    }
}

fn error_to_wire(context: &RequestContext, error: McpPlatformError) -> McpPlatformErrorEnvelope {
    use crate::mcp_platform::McpPlatformErrorCode as Code;

    let (code, message, retryable, details) = match error.code() {
        Code::NotFound => (
            McpPlatformErrorCodeDto::NotFound,
            "The requested MCP platform record was not found.",
            false,
            Some(McpPlatformErrorDetails::RecordMissing {}),
        ),
        Code::IntegrityError => (
            McpPlatformErrorCodeDto::IntegrityError,
            "Stored MCP platform data failed integrity validation.",
            false,
            Some(McpPlatformErrorDetails::IntegrityValidationFailed {}),
        ),
        Code::PolicyDenied => (
            McpPlatformErrorCodeDto::PolicyDenied,
            "The MCP platform policy denied this request.",
            false,
            Some(McpPlatformErrorDetails::PolicyDecision {
                reason_codes: Vec::new(),
            }),
        ),
        Code::NotImplementedForPhase => (
            McpPlatformErrorCodeDto::NotImplementedForPhase,
            "This distribution is visible but cannot be planned in phase 2C.",
            false,
            Some(McpPlatformErrorDetails::PhaseUnavailable {
                phase: "2C".to_string(),
                operation: "plan".to_string(),
            }),
        ),
        Code::OperationNotSupported => (
            McpPlatformErrorCodeDto::OperationNotSupported,
            "This lifecycle operation is not supported in phase 2C.",
            false,
            Some(McpPlatformErrorDetails::PhaseUnavailable {
                phase: "2C".to_string(),
                operation: "lifecycle".to_string(),
            }),
        ),
        Code::PlanStale | Code::PlanConflict => (
            McpPlatformErrorCodeDto::PlanStale,
            "The reviewed plan no longer matches the confirmation request.",
            false,
            Some(McpPlatformErrorDetails::PlanMismatch {}),
        ),
        Code::PlanExpired => (
            McpPlatformErrorCodeDto::PlanExpired,
            "The reviewed plan has expired.",
            false,
            Some(McpPlatformErrorDetails::PlanExpired {}),
        ),
        Code::IdempotencyConflict => (
            McpPlatformErrorCodeDto::IdempotencyConflict,
            "The idempotency key already refers to different content.",
            false,
            Some(McpPlatformErrorDetails::IdempotencyConflict {}),
        ),
        Code::RevisionConflict => (
            McpPlatformErrorCodeDto::ProjectionConflict,
            "The record revision changed; reload before retrying.",
            true,
            Some(McpPlatformErrorDetails::ProjectionConflict {}),
        ),
        Code::InvalidTransition => (
            McpPlatformErrorCodeDto::InvalidTransition,
            "The task state does not permit this transition.",
            false,
            Some(McpPlatformErrorDetails::TransitionRejected {}),
        ),
        Code::RepositoryUnavailable | Code::SchemaTooNew => (
            McpPlatformErrorCodeDto::RepositoryUnavailable,
            "The MCP platform repository is temporarily unavailable.",
            true,
            Some(McpPlatformErrorDetails::RepositoryTemporarilyUnavailable {}),
        ),
        Code::ProjectionConflict => (
            McpPlatformErrorCodeDto::RevisionConflict,
            "The managed MCP projection conflicts with an existing extension.",
            false,
            Some(McpPlatformErrorDetails::RevisionConflict {}),
        ),
        Code::CredentialMissing => (
            McpPlatformErrorCodeDto::CredentialMissing,
            "A required credential handle is missing.",
            false,
            Some(McpPlatformErrorDetails::CredentialMissing {}),
        ),
        Code::HealthFailed => (
            McpPlatformErrorCodeDto::HealthFailed,
            "The MCP health check failed.",
            true,
            Some(McpPlatformErrorDetails::HealthGateFailed {}),
        ),
        Code::TaskNotCancellable => (
            McpPlatformErrorCodeDto::TaskNotCancellable,
            "Task cancellation is deferred to a safe boundary.",
            false,
            Some(McpPlatformErrorDetails::CancellationDeferred {}),
        ),
        Code::RollbackIncomplete => (
            McpPlatformErrorCodeDto::RollbackIncomplete,
            "The lifecycle mutation requires durable recovery.",
            false,
            Some(McpPlatformErrorDetails::RecoveryRequired {}),
        ),
        Code::AdapterIncompatible => (
            McpPlatformErrorCodeDto::AdapterIncompatible,
            "The journal adapter version is incompatible with this worker.",
            false,
            Some(McpPlatformErrorDetails::AdapterVersionIncompatible {}),
        ),
        Code::InvalidRequest
        | Code::InvalidJson
        | Code::InvalidManifest
        | Code::UnsupportedSchema
        | Code::VersionNotExact
        | Code::UnsafeUrl
        | Code::InvalidDigest
        | Code::ImmutableReferenceRequired
        | Code::DuplicateSelector
        | Code::TransportMismatch
        | Code::PathTraversal
        | Code::UnknownTemplateVariable
        | Code::UnknownAdapter
        | Code::ManifestConflict
        | Code::SerializationFailed => (
            McpPlatformErrorCodeDto::InvalidRequest,
            "The MCP platform request is invalid.",
            false,
            None,
        ),
    };
    McpPlatformErrorEnvelope {
        code,
        message: message.to_string(),
        retryable,
        correlation_id: context.correlation_id().to_string(),
        details,
    }
}

fn catalog_page_to_wire(page: crate::mcp_platform::service::CatalogPage) -> McpCatalogPage {
    McpCatalogPage {
        items: page
            .items
            .into_iter()
            .map(catalog_summary_to_wire)
            .collect(),
        next_cursor: page.next_cursor,
        cache: McpCatalogCacheMetadata {
            offline: page.offline,
            local_persistence_only: page.local_persistence_only,
            newest_verified_at_ms: page.newest_verified_at_ms,
        },
    }
}

fn catalog_summary_to_wire(
    summary: crate::mcp_platform::service::CatalogSummary,
) -> McpCatalogSummary {
    McpCatalogSummary {
        source_id: summary.source_id,
        mcp_id: summary.mcp_id,
        version: summary.version,
        manifest_digest: summary.manifest_digest,
        name: summary.name,
        description: summary.description,
        publisher_id: summary.publisher_id,
        publisher_name: summary.publisher_name,
        trust_tier: trust_to_wire(summary.trust_tier),
        proof: proof_to_wire(summary.proof),
        compatibility: compatibility_to_wire(summary.compatibility),
        distribution: distribution_kind(&summary.distribution_adapter),
        verified_at_ms: summary.verified_at_ms,
    }
}

fn catalog_detail_to_wire(detail: crate::mcp_platform::service::CatalogDetail) -> McpCatalogDetail {
    let manifest = detail.manifest;
    McpCatalogDetail {
        source_id: detail.source_id,
        mcp_id: manifest.id.clone(),
        version: manifest.version.as_str().to_string(),
        manifest_digest: detail.manifest_digest,
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        publisher: publisher_to_wire(&manifest),
        proof: proof_to_wire(detail.proof),
        trust_tier: trust_to_wire(detail.trust_tier),
        distribution: distribution_to_wire(&manifest.distribution),
        transport: transport_to_wire(&manifest.transport),
        auth: auth_to_wire(&manifest.auth),
        health: health_to_wire(&manifest.health_check),
        capabilities: manifest
            .capabilities
            .iter()
            .copied()
            .map(capability_to_wire)
            .collect(),
        permissions: manifest
            .permissions
            .iter()
            .map(permission_to_wire)
            .collect(),
        compatibility: compatibility_to_wire(detail.compatibility),
        verified_at_ms: detail.verified_at_ms,
    }
}

fn plan_review_to_wire(review: crate::mcp_platform::service::PlanReview) -> McpPlanReview {
    let manifest = review.manifest;
    McpPlanReview {
        plan_id: review.plan_id,
        plan_digest: review.plan_digest,
        expires_at_ms: review.expires_at_ms,
        source_id: review.source_id,
        proof: proof_to_wire(review.proof),
        trust_tier: trust_to_wire(review.trust_tier),
        publisher: publisher_to_wire(&manifest),
        mcp_id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.as_str().to_string(),
        permissions: manifest
            .permissions
            .iter()
            .map(permission_to_wire)
            .collect(),
        network_origins: review.plan.effects().network_origins.clone(),
        file_effects: McpFileEffects {
            writes_files: review.plan.effects().writes_files,
            removes_files: review.plan.effects().removes_files,
            owned_items: manifest.owned_files.len() as u32,
        },
        host_effects: McpHostEffects {
            registration_ids: manifest
                .host_integrations
                .iter()
                .map(|integration| integration.id.clone())
                .collect(),
        },
        process_effects: McpProcessEffects {
            process_required_for_connection: review.plan.effects().requires_process_spawn,
            starts_during_confirmation: false,
        },
        reversibility: McpReversibility {
            reversible: true,
            rollback_summary: "Registration can be cancelled without installation or file effects."
                .to_string(),
        },
        policy: McpPolicyReview {
            outcome: policy_outcome_to_wire(review.policy.outcome),
            reasons: review
                .policy
                .reasons
                .into_iter()
                .map(|reason| McpPolicyReason {
                    code: reason.code.as_str().to_string(),
                    message: reason.message,
                })
                .collect(),
        },
        warnings: review
            .plan
            .warnings()
            .iter()
            .map(|warning| match warning {
                crate::mcp_platform::PlanWarning::DefaultDisabled => {
                    McpPlanWarning::DefaultDisabled
                }
                crate::mcp_platform::PlanWarning::RegistrationOnly => {
                    McpPlanWarning::RegistrationOnly
                }
            })
            .collect(),
        required_confirmations: review
            .plan
            .required_confirmations()
            .iter()
            .map(|confirmation| match confirmation {
                crate::mcp_platform::RequiredConfirmation::Policy { reason_code } => {
                    McpRequiredConfirmation::Policy {
                        reason_code: reason_code.as_str().to_string(),
                    }
                }
                crate::mcp_platform::RequiredConfirmation::Permission { permission_id } => {
                    McpRequiredConfirmation::Permission {
                        permission_id: permission_id.clone(),
                    }
                }
            })
            .collect(),
        default_disabled: !review.plan.default_enabled(),
    }
}

fn task_to_wire(task: crate::mcp_platform::service::TaskRef) -> McpTaskRef {
    McpTaskRef {
        task_id: task.task_id,
        operation: task_operation_to_wire(task.operation),
        status: task_status_to_wire(task.status),
        progress: task.progress,
        cancellable: task.cancellable,
        revision: task.revision,
        updated_at_ms: task.updated_at_ms,
    }
}

fn events_to_wire(page: crate::mcp_platform::service::EventsResumePage) -> McpEventsPage {
    McpEventsPage {
        events: page
            .events
            .into_iter()
            .map(|event| McpEventEnvelope {
                event_id: event.event_id,
                task_id: event.task_id,
                task_local_sequence: event.sequence,
                occurred_at_ms: event.occurred_at_ms,
                actor: event.actor,
                event_type: event_type_to_wire(event.event_type),
                payload: event_payload_to_wire(event.payload),
            })
            .collect(),
        next_event_id: page.next_event_id,
        tasks: page.tasks.into_iter().map(task_to_wire).collect(),
    }
}

fn proof_to_wire(proof: ManifestProof) -> McpSourceProof {
    match proof {
        ManifestProof::LocalBytes => McpSourceProof::LocalBytes,
        ManifestProof::Catalog {
            index_digest,
            declared_manifest_digest,
            signature,
        } => McpSourceProof::Catalog {
            index_digest,
            declared_manifest_digest,
            signature: signature.map(|signature| McpSignatureProof {
                algorithm: signature.algorithm,
                signing_identity: signature.signing_identity,
                present: true,
            }),
        },
    }
}

fn publisher_to_wire(manifest: &Manifest) -> McpPublisher {
    McpPublisher {
        id: manifest.publisher.id.clone(),
        name: manifest.publisher.name.clone(),
        website: manifest.publisher.website.clone(),
        signing_identities: manifest.publisher.signing_identities.clone(),
    }
}

fn distribution_to_wire(distribution: &Distribution) -> McpDistributionContract {
    match distribution {
        Distribution::RemoteHttp => McpDistributionContract::RemoteHttp,
        Distribution::ManualStdio { platforms, .. } => McpDistributionContract::ManualStdio {
            platforms: platforms.iter().copied().map(platform_name).collect(),
        },
        Distribution::Npm {
            package,
            package_version,
            artifacts,
            ..
        } => McpDistributionContract::Npm {
            package: package.clone(),
            package_version: package_version.as_str().to_string(),
            artifacts: artifacts.iter().map(artifact_to_wire).collect(),
        },
        Distribution::PythonWheel {
            package,
            package_version,
            python,
            artifacts,
            ..
        } => McpDistributionContract::PythonWheel {
            package: package.clone(),
            package_version: package_version.as_str().to_string(),
            python: python.clone(),
            artifacts: artifacts.iter().map(artifact_to_wire).collect(),
        },
        Distribution::BinaryArchive {
            archive_format,
            artifacts,
            ..
        } => McpDistributionContract::BinaryArchive {
            archive_format: archive_format_name(*archive_format).to_string(),
            artifacts: artifacts.iter().map(artifact_to_wire).collect(),
        },
        Distribution::Docker { image, digest, .. } => McpDistributionContract::Docker {
            image: image.clone(),
            digest: digest.value.clone(),
        },
        Distribution::GitDev {
            repository,
            commit,
            adapter,
            ..
        } => McpDistributionContract::GitDev {
            repository: repository.clone(),
            commit: commit.clone(),
            adapter: git_adapter_name(*adapter).to_string(),
        },
    }
}

fn artifact_to_wire(artifact: &crate::mcp_platform::manifest::Artifact) -> McpArtifactContract {
    McpArtifactContract {
        platform: platform_name(artifact.platform),
        architecture: architecture_name(artifact.arch),
        origin: artifact.url.clone(),
        digest: artifact.digest.value.clone(),
        size_bytes: artifact.size_bytes,
    }
}

fn transport_to_wire(transport: &Transport) -> McpTransportContract {
    match transport {
        Transport::Stdio {
            startup_timeout_seconds,
        } => McpTransportContract::Stdio {
            startup_timeout_seconds: *startup_timeout_seconds,
        },
        Transport::StreamableHttp {
            url,
            connect_timeout_seconds,
            allowed_redirect_origins,
        } => McpTransportContract::StreamableHttp {
            endpoint: url.clone(),
            connect_timeout_seconds: *connect_timeout_seconds,
            allowed_redirect_origins: allowed_redirect_origins.clone(),
        },
    }
}

fn auth_to_wire(auth: &Auth) -> McpAuthContract {
    match auth {
        Auth::None => McpAuthContract::None,
        Auth::ApiKeyHeader {
            header_name,
            prefix,
            credential_name,
        } => McpAuthContract::ApiKeyHeader {
            header_name: header_name.clone(),
            credential_name: credential_name.clone(),
            prefix_required: prefix.is_some(),
        },
        Auth::Environment {
            environment_key,
            credential_name,
        } => McpAuthContract::Environment {
            environment_key: environment_key.clone(),
            credential_name: credential_name.clone(),
        },
        Auth::Oauth2 {
            authorization_url,
            token_url,
            client_registration,
            scopes,
        } => McpAuthContract::Oauth2 {
            authorization_endpoint: authorization_url.clone(),
            token_endpoint: token_url.clone(),
            client_registration: match client_registration {
                OAuthClientRegistration::Dynamic => "dynamic",
                OAuthClientRegistration::UserProvided => "user_provided",
            }
            .to_string(),
            scopes: scopes.clone(),
        },
    }
}

fn health_to_wire(health: &HealthCheck) -> McpHealthContract {
    match health {
        HealthCheck::McpInitialize { timeout_seconds } => McpHealthContract::McpInitialize {
            timeout_seconds: *timeout_seconds,
        },
        HealthCheck::McpListTools { timeout_seconds } => McpHealthContract::McpListTools {
            timeout_seconds: *timeout_seconds,
        },
        HealthCheck::Http {
            path,
            expected_status,
            timeout_seconds,
        } => McpHealthContract::Http {
            relative_endpoint: path.clone(),
            expected_status: *expected_status,
            timeout_seconds: *timeout_seconds,
        },
    }
}

fn permission_to_wire(permission: &crate::mcp_platform::manifest::Permission) -> McpPermission {
    McpPermission {
        id: permission.id.clone(),
        kind: match permission.kind {
            PermissionKind::FilesystemRead => McpPermissionKind::FilesystemRead,
            PermissionKind::FilesystemWrite => McpPermissionKind::FilesystemWrite,
            PermissionKind::Network => McpPermissionKind::Network,
            PermissionKind::Credentials => McpPermissionKind::Credentials,
            PermissionKind::ProcessSpawn => McpPermissionKind::ProcessSpawn,
            PermissionKind::Docker => McpPermissionKind::Docker,
            PermissionKind::HostApplication => McpPermissionKind::HostApplication,
        },
        reason: permission.reason.clone(),
        required: permission.required,
        scope: permission.scope.clone(),
    }
}

fn capability_to_wire(capability: Capability) -> McpCapability {
    match capability {
        Capability::Tools => McpCapability::Tools,
        Capability::Resources => McpCapability::Resources,
        Capability::Prompts => McpCapability::Prompts,
        Capability::Sampling => McpCapability::Sampling,
        Capability::Elicitation => McpCapability::Elicitation,
        Capability::Logging => McpCapability::Logging,
    }
}

fn trust_from_wire(trust: McpTrustTier) -> crate::mcp_platform::TrustTier {
    match trust {
        McpTrustTier::Official => crate::mcp_platform::TrustTier::Official,
        McpTrustTier::Community => crate::mcp_platform::TrustTier::Community,
        McpTrustTier::Local => crate::mcp_platform::TrustTier::Local,
    }
}

fn trust_to_wire(trust: crate::mcp_platform::TrustTier) -> McpTrustTier {
    match trust {
        crate::mcp_platform::TrustTier::Official => McpTrustTier::Official,
        crate::mcp_platform::TrustTier::Community => McpTrustTier::Community,
        crate::mcp_platform::TrustTier::Local => McpTrustTier::Local,
    }
}

fn compatibility_from_wire(
    compatibility: McpCompatibility,
) -> crate::mcp_platform::CatalogCompatibility {
    match compatibility {
        McpCompatibility::Compatible => crate::mcp_platform::CatalogCompatibility::Compatible,
        McpCompatibility::Incompatible => crate::mcp_platform::CatalogCompatibility::Incompatible,
    }
}

fn compatibility_to_wire(
    compatibility: crate::mcp_platform::CatalogCompatibility,
) -> McpCompatibility {
    match compatibility {
        crate::mcp_platform::CatalogCompatibility::Compatible => McpCompatibility::Compatible,
        crate::mcp_platform::CatalogCompatibility::Incompatible => McpCompatibility::Incompatible,
    }
}

fn distribution_kind(adapter: &str) -> McpDistributionKind {
    match adapter {
        "remote_http" => McpDistributionKind::RemoteHttp,
        "manual_stdio" => McpDistributionKind::ManualStdio,
        "npm" => McpDistributionKind::Npm,
        "python_wheel" => McpDistributionKind::PythonWheel,
        "binary_archive" => McpDistributionKind::BinaryArchive,
        "docker" => McpDistributionKind::Docker,
        "git_dev" => McpDistributionKind::GitDev,
        _ => unreachable!("verified manifests use a closed distribution enum"),
    }
}

fn policy_outcome_to_wire(outcome: PolicyOutcome) -> McpPolicyOutcome {
    match outcome {
        PolicyOutcome::Allow => McpPolicyOutcome::Allow,
        PolicyOutcome::Deny => McpPolicyOutcome::Deny,
        PolicyOutcome::NeedsConfirmation => McpPolicyOutcome::NeedsConfirmation,
    }
}

fn task_operation_to_wire(operation: TaskOperation) -> McpTaskOperation {
    match operation {
        TaskOperation::Register => McpTaskOperation::Register,
        TaskOperation::Install => McpTaskOperation::Install,
        TaskOperation::Update => McpTaskOperation::Update,
        TaskOperation::Repair => McpTaskOperation::Repair,
        TaskOperation::Uninstall => McpTaskOperation::Uninstall,
        TaskOperation::Health => McpTaskOperation::Health,
    }
}

fn task_status_to_wire(status: TaskStatus) -> McpTaskStatus {
    match status {
        TaskStatus::Planned => McpTaskStatus::Planned,
        TaskStatus::AwaitingConfirmation => McpTaskStatus::AwaitingConfirmation,
        TaskStatus::Queued => McpTaskStatus::Queued,
        TaskStatus::Running => McpTaskStatus::Running,
        TaskStatus::Cancelling => McpTaskStatus::Cancelling,
        TaskStatus::Verifying => McpTaskStatus::Verifying,
        TaskStatus::Activating => McpTaskStatus::Activating,
        TaskStatus::RollingBack => McpTaskStatus::RollingBack,
        TaskStatus::Succeeded => McpTaskStatus::Succeeded,
        TaskStatus::Failed => McpTaskStatus::Failed,
        TaskStatus::Cancelled => McpTaskStatus::Cancelled,
        TaskStatus::Interrupted => McpTaskStatus::Interrupted,
        TaskStatus::RecoveryRequired => McpTaskStatus::RecoveryRequired,
    }
}

fn event_type_to_wire(event_type: AuditEventType) -> McpEventType {
    match event_type {
        AuditEventType::TaskCreated => McpEventType::TaskCreated,
        AuditEventType::TaskStatusChanged => McpEventType::TaskStatusChanged,
        AuditEventType::StepStarted => McpEventType::StepStarted,
        AuditEventType::StepCommitted => McpEventType::StepCommitted,
        AuditEventType::RecoveryInterrupted => McpEventType::RecoveryInterrupted,
        AuditEventType::ConfirmationRecorded => McpEventType::ConfirmationRecorded,
        AuditEventType::CancellationRequested => McpEventType::CancellationRequested,
        AuditEventType::RetryQueued => McpEventType::RetryQueued,
    }
}

fn event_payload_to_wire(payload: AuditPayload) -> McpEventPayload {
    match payload {
        AuditPayload::TaskCreated { status } => McpEventPayload::TaskCreated {
            status: task_status_to_wire(status),
        },
        AuditPayload::TaskStatusChanged { from, to } => McpEventPayload::TaskStatusChanged {
            from: task_status_to_wire(from),
            to: task_status_to_wire(to),
        },
        AuditPayload::ConfirmationRecorded {
            from,
            to,
            plan_id,
            plan_digest,
        } => McpEventPayload::ConfirmationRecorded {
            from: task_status_to_wire(from),
            to: task_status_to_wire(to),
            plan_id,
            plan_digest,
        },
        AuditPayload::CancellationRequested { from, to } => {
            McpEventPayload::CancellationRequested {
                from: task_status_to_wire(from),
                to: task_status_to_wire(to),
            }
        }
        AuditPayload::StepStatusChanged { ordinal, from, to } => {
            McpEventPayload::StepStatusChanged {
                ordinal,
                from: task_step_to_wire(from),
                to: task_step_to_wire(to),
            }
        }
        AuditPayload::RecoveryDecision { decision } => McpEventPayload::RecoveryDecision {
            decision: match decision {
                RecoveryDecision::ResumeFromStep { ordinal } => {
                    McpRecoveryDecision::ResumeFromStep { ordinal }
                }
                RecoveryDecision::RollbackFromStep { ordinal } => {
                    McpRecoveryDecision::RollbackFromStep { ordinal }
                }
                RecoveryDecision::RequiresManualRecovery => {
                    McpRecoveryDecision::RequiresManualRecovery
                }
            },
        },
    }
}

fn task_step_to_wire(status: TaskStepStatus) -> McpTaskStepStatus {
    match status {
        TaskStepStatus::NotStarted => McpTaskStepStatus::NotStarted,
        TaskStepStatus::Started => McpTaskStepStatus::Started,
        TaskStepStatus::Committed => McpTaskStepStatus::Committed,
    }
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

const fn archive_format_name(format: ArchiveFormat) -> &'static str {
    match format {
        ArchiveFormat::Zip => "zip",
        ArchiveFormat::TarGz => "tar.gz",
        ArchiveFormat::TarXz => "tar.xz",
    }
}

const fn git_adapter_name(adapter: GitDevAdapter) -> &'static str {
    match adapter {
        GitDevAdapter::Npm => "npm",
        GitDevAdapter::PythonWheel => "python_wheel",
        GitDevAdapter::BinaryArchive => "binary_archive",
        GitDevAdapter::Docker => "docker",
    }
}

fn managed_page_to_wire(page: crate::mcp_platform::service::ManagedMcpPage) -> McpManagedPage {
    McpManagedPage {
        items: page
            .items
            .into_iter()
            .map(managed_summary_to_wire)
            .collect(),
        next_cursor: page.next_cursor,
    }
}

fn managed_summary_to_wire(
    value: crate::mcp_platform::service::ManagedMcpSummary,
) -> McpManagedSummary {
    McpManagedSummary {
        managed_mcp_id: value.managed_mcp_id,
        mcp_id: value.mcp_id,
        installation_scope: McpInstallationScope::User,
        registration: registration_to_wire(value.registration),
        installation: installation_to_wire(value.installation),
        runtime: runtime_to_wire(value.runtime),
        health: health_state_to_wire(value.health),
        default_enabled: value.default_enabled,
        revision: value.revision,
        updated_at_ms: value.updated_at_ms,
    }
}

fn managed_detail_to_wire(
    value: crate::mcp_platform::service::ManagedMcpDetail,
) -> McpManagedDetail {
    McpManagedDetail {
        summary: managed_summary_to_wire(value.summary),
        distribution_adapter: value.distribution_adapter,
        active_manifest_digest: value.active_manifest_digest,
        active_version: value.active_version,
        extension_config_key: value.extension_config_key,
        projection_digest: value.projection_digest,
        latest_health: value.latest_health.map(health_observation_to_wire),
        registration_task: value.registration_task.map(task_to_wire),
    }
}

fn health_status_to_wire(value: crate::mcp_platform::service::HealthStatus) -> McpHealthStatus {
    McpHealthStatus {
        managed_mcp_id: value.managed_mcp_id,
        state: health_state_to_wire(value.state),
        latest: value.latest.map(health_observation_to_wire),
    }
}

fn health_observation_to_wire(
    value: crate::mcp_platform::HealthObservationRecord,
) -> McpHealthObservation {
    McpHealthObservation {
        task_id: value.task_id,
        mode: if value.check_type == "runtime" {
            McpHealthCheckMode::Runtime
        } else {
            McpHealthCheckMode::Registration
        },
        result: match value.result_code {
            crate::mcp_platform::HealthResultCode::Healthy => McpHealthResult::Healthy,
            crate::mcp_platform::HealthResultCode::Unhealthy => McpHealthResult::Unhealthy,
            crate::mcp_platform::HealthResultCode::BlockedAuth => McpHealthResult::BlockedAuth,
            crate::mcp_platform::HealthResultCode::Incompatible => McpHealthResult::Incompatible,
            crate::mcp_platform::HealthResultCode::Timeout => McpHealthResult::Timeout,
            crate::mcp_platform::HealthResultCode::Cancelled => McpHealthResult::Cancelled,
        },
        latency_ms: value.latency_ms,
        capabilities_digest: value.capabilities_digest,
        tools_digest: value.tools_digest,
        checked_at_ms: value.checked_at_ms,
        detail_code: match value.detail_code {
            crate::mcp_platform::HealthDetailCode::ProjectionConsistent => {
                McpHealthDetailCode::ProjectionConsistent
            }
            crate::mcp_platform::HealthDetailCode::ProjectionDrift => {
                McpHealthDetailCode::ProjectionDrift
            }
            crate::mcp_platform::HealthDetailCode::CredentialHandleMissing => {
                McpHealthDetailCode::CredentialHandleMissing
            }
            crate::mcp_platform::HealthDetailCode::ExpectedStatus => {
                McpHealthDetailCode::ExpectedStatus
            }
            crate::mcp_platform::HealthDetailCode::UnexpectedStatus => {
                McpHealthDetailCode::UnexpectedStatus
            }
            crate::mcp_platform::HealthDetailCode::McpInitializeSucceeded => {
                McpHealthDetailCode::McpInitializeSucceeded
            }
            crate::mcp_platform::HealthDetailCode::McpInitializeFailed => {
                McpHealthDetailCode::McpInitializeFailed
            }
            crate::mcp_platform::HealthDetailCode::McpListToolsSucceeded => {
                McpHealthDetailCode::McpListToolsSucceeded
            }
            crate::mcp_platform::HealthDetailCode::McpListToolsFailed => {
                McpHealthDetailCode::McpListToolsFailed
            }
            crate::mcp_platform::HealthDetailCode::IncompatibleHealthContract => {
                McpHealthDetailCode::IncompatibleHealthContract
            }
            crate::mcp_platform::HealthDetailCode::CleanupFailed => {
                McpHealthDetailCode::CleanupFailed
            }
            crate::mcp_platform::HealthDetailCode::Timeout => McpHealthDetailCode::Timeout,
            crate::mcp_platform::HealthDetailCode::Cancelled => McpHealthDetailCode::Cancelled,
        },
    }
}

fn registration_from_wire(value: McpRegistrationState) -> crate::mcp_platform::RegistrationState {
    match value {
        McpRegistrationState::Absent => crate::mcp_platform::RegistrationState::Absent,
        McpRegistrationState::Registered => crate::mcp_platform::RegistrationState::Registered,
    }
}
fn registration_to_wire(value: crate::mcp_platform::RegistrationState) -> McpRegistrationState {
    match value {
        crate::mcp_platform::RegistrationState::Absent => McpRegistrationState::Absent,
        crate::mcp_platform::RegistrationState::Registered => McpRegistrationState::Registered,
    }
}
fn installation_from_wire(value: McpInstallationState) -> crate::mcp_platform::InstallationState {
    match value {
        McpInstallationState::NotApplicable => {
            crate::mcp_platform::InstallationState::NotApplicable
        }
        McpInstallationState::NotInstalled => crate::mcp_platform::InstallationState::NotInstalled,
        McpInstallationState::Staged => crate::mcp_platform::InstallationState::Staged,
        McpInstallationState::Installed => crate::mcp_platform::InstallationState::Installed,
        McpInstallationState::UpdateAvailable => {
            crate::mcp_platform::InstallationState::UpdateAvailable
        }
        McpInstallationState::RepairRequired => {
            crate::mcp_platform::InstallationState::RepairRequired
        }
        McpInstallationState::UninstallPending => {
            crate::mcp_platform::InstallationState::UninstallPending
        }
    }
}
fn installation_to_wire(value: crate::mcp_platform::InstallationState) -> McpInstallationState {
    match value {
        crate::mcp_platform::InstallationState::NotApplicable => {
            McpInstallationState::NotApplicable
        }
        crate::mcp_platform::InstallationState::NotInstalled => McpInstallationState::NotInstalled,
        crate::mcp_platform::InstallationState::Staged => McpInstallationState::Staged,
        crate::mcp_platform::InstallationState::Installed => McpInstallationState::Installed,
        crate::mcp_platform::InstallationState::UpdateAvailable => {
            McpInstallationState::UpdateAvailable
        }
        crate::mcp_platform::InstallationState::RepairRequired => {
            McpInstallationState::RepairRequired
        }
        crate::mcp_platform::InstallationState::UninstallPending => {
            McpInstallationState::UninstallPending
        }
    }
}
fn runtime_from_wire(value: McpRuntimeState) -> crate::mcp_platform::RuntimeState {
    match value {
        McpRuntimeState::Stopped => crate::mcp_platform::RuntimeState::Stopped,
        McpRuntimeState::Starting => crate::mcp_platform::RuntimeState::Starting,
        McpRuntimeState::Running => crate::mcp_platform::RuntimeState::Running,
        McpRuntimeState::Stopping => crate::mcp_platform::RuntimeState::Stopping,
        McpRuntimeState::Crashed => crate::mcp_platform::RuntimeState::Crashed,
    }
}
fn runtime_to_wire(value: crate::mcp_platform::RuntimeState) -> McpRuntimeState {
    match value {
        crate::mcp_platform::RuntimeState::Stopped => McpRuntimeState::Stopped,
        crate::mcp_platform::RuntimeState::Starting => McpRuntimeState::Starting,
        crate::mcp_platform::RuntimeState::Running => McpRuntimeState::Running,
        crate::mcp_platform::RuntimeState::Stopping => McpRuntimeState::Stopping,
        crate::mcp_platform::RuntimeState::Crashed => McpRuntimeState::Crashed,
    }
}
fn health_from_wire(value: McpHealthState) -> crate::mcp_platform::HealthState {
    match value {
        McpHealthState::Unknown => crate::mcp_platform::HealthState::Unknown,
        McpHealthState::Checking => crate::mcp_platform::HealthState::Checking,
        McpHealthState::Healthy => crate::mcp_platform::HealthState::Healthy,
        McpHealthState::Degraded => crate::mcp_platform::HealthState::Degraded,
        McpHealthState::Unhealthy => crate::mcp_platform::HealthState::Unhealthy,
        McpHealthState::BlockedAuth => crate::mcp_platform::HealthState::BlockedAuth,
        McpHealthState::Incompatible => crate::mcp_platform::HealthState::Incompatible,
    }
}
fn health_state_to_wire(value: crate::mcp_platform::HealthState) -> McpHealthState {
    match value {
        crate::mcp_platform::HealthState::Unknown => McpHealthState::Unknown,
        crate::mcp_platform::HealthState::Checking => McpHealthState::Checking,
        crate::mcp_platform::HealthState::Healthy => McpHealthState::Healthy,
        crate::mcp_platform::HealthState::Degraded => McpHealthState::Degraded,
        crate::mcp_platform::HealthState::Unhealthy => McpHealthState::Unhealthy,
        crate::mcp_platform::HealthState::BlockedAuth => McpHealthState::BlockedAuth,
        crate::mcp_platform::HealthState::Incompatible => McpHealthState::Incompatible,
    }
}
