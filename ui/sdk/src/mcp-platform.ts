import type { ExtMethodProvider } from "./generated/client.gen.js";

export type McpTrustTier = "official" | "community" | "local";
export type McpCompatibility = "compatible" | "incompatible";
export type McpRecoverySuggestion =
  | "none"
  | "review_permissions"
  | "choose_compatible_release"
  | "install_required_runtime"
  | "enable_development_mode"
  | "restore_verified_cache"
  | "retry"
  | "recreate_plan"
  | "resolve_recovery"
  | "contact_policy_administrator";
export type McpEligibilityOutcome = "allowed" | "restricted" | "denied";
export type McpEligibilityReason =
  | "eligible"
  | "confirmation_required"
  | "platform_unsupported"
  | "runtime_unavailable"
  | "policy_denied"
  | "development_mode_required"
  | "external_capability_unavailable";
export interface McpEligibility {
  outcome: McpEligibilityOutcome;
  reason: McpEligibilityReason;
  recovery: McpRecoverySuggestion;
}

export type McpSourceProof =
  | { type: "local_bytes" }
  | {
      type: "catalog";
      index_digest: string;
      declared_manifest_digest: string;
      signature?: {
        algorithm: string;
        signingIdentity: string;
        present: boolean;
      };
    };
export type McpDistributionKind =
  | "remote_http"
  | "manual_stdio"
  | "npm"
  | "python_wheel"
  | "binary_archive"
  | "docker"
  | "git_dev";
export interface McpPublisher {
  id: string;
  name: string;
  website?: string;
  signingIdentities: string[];
}
export interface McpCatalogCacheMetadata {
  offline: boolean;
  localPersistenceOnly: boolean;
  newestVerifiedAtMs?: number;
  freshness: "fresh" | "stale" | "offline_verified" | "empty";
  refreshState: "idle" | "refreshing" | "failed" | "local_only";
  recovery: McpRecoverySuggestion;
}
export type McpGovernedImportKind =
  | "local_persistence"
  | "verified_source_catalog"
  | "https_manifest_url"
  | "enterprise_directory";
export type McpGovernedCatalogDocumentKind = "manifest" | "directory";
export interface McpGovernedCatalogSourceSummary {
  sourceId: string;
  importKind: McpGovernedImportKind;
  displayName: string;
}
export interface McpGovernedCatalogDocumentSummary {
  sourceId: string;
  documentId: string;
  documentDigest: string;
  documentKind: McpGovernedCatalogDocumentKind;
}
export interface McpGovernedImportResult {
  source: McpGovernedCatalogSourceSummary;
  document: McpGovernedCatalogDocumentSummary;
  entries: McpCatalogSummary[];
}
export type McpSourceProvisionRefreshTransport = "verified_source_bundle_v1";
export type McpSourceProvisionTrustBasis = "user_pin";
export interface McpSourceProvisionPreview {
  sourceId: string;
  displayName: string;
  rootDigest: string;
  documentDigest: string;
  endpointHost: string;
  refreshTransport: McpSourceProvisionRefreshTransport;
  manifestCount: number;
  warnings: string[];
  trustBasis: McpSourceProvisionTrustBasis;
}
export interface McpSourceProvisionPrepareResult {
  provisionId: string;
  confirmationToken: string;
  expiresAtMs: number;
  preview: McpSourceProvisionPreview;
}
export interface McpSourceProvisionConfirmResult {
  provisionId: string;
  confirmedAtMs: number;
  preview: McpSourceProvisionPreview;
}
export interface McpCatalogSummary {
  sourceId: string;
  mcpId: string;
  version: string;
  manifestDigest: string;
  name: string;
  description: string;
  publisherId: string;
  publisherName: string;
  trustTier: McpTrustTier;
  proof: McpSourceProof;
  compatibility: McpCompatibility;
  distribution: McpDistributionKind;
  verifiedAtMs: number;
  eligibility: McpEligibility;
}
export interface McpCatalogPage {
  items: McpCatalogSummary[];
  nextCursor?: string;
  cache: McpCatalogCacheMetadata;
}
export type McpDistributionContract =
  | { type: "remote_http" }
  | { type: "manual_stdio"; platforms: string[] }
  | {
      type: "npm";
      package: string;
      package_version: string;
      artifacts: McpArtifactContract[];
    }
  | {
      type: "python_wheel";
      package: string;
      package_version: string;
      python: string;
      artifacts: McpArtifactContract[];
    }
  | {
      type: "binary_archive";
      archive_format: string;
      artifacts: McpArtifactContract[];
    }
  | { type: "docker"; image: string; digest: string }
  | { type: "git_dev"; repository: string; commit: string; adapter: string };
export interface McpArtifactContract {
  platform: string;
  architecture: string;
  origin: string;
  digest: string;
  sizeBytes?: number;
}
export type McpTransportContract =
  | { type: "stdio"; startup_timeout_seconds?: number }
  | {
      type: "streamable_http";
      endpoint: string;
      connect_timeout_seconds?: number;
      allowed_redirect_origins: string[];
    };
export type McpAuthContract =
  | { type: "none" }
  | {
      type: "api_key_header";
      credential_required: boolean;
      prefix_required: boolean;
    }
  | { type: "environment"; credential_required: boolean }
  | {
      type: "oauth2";
      authorization_endpoint: string;
      token_endpoint: string;
      client_registration: string;
      scopes: string[];
    };
export type McpHealthContract =
  | { type: "mcp_initialize"; timeout_seconds: number }
  | { type: "mcp_list_tools"; timeout_seconds: number }
  | {
      type: "http";
      relative_endpoint: string;
      expected_status: number;
      timeout_seconds: number;
    };
export type McpCapability =
  | "tools"
  | "resources"
  | "prompts"
  | "sampling"
  | "elicitation"
  | "logging";
export type McpPermissionKind =
  | "filesystem_read"
  | "filesystem_write"
  | "network"
  | "credentials"
  | "process_spawn"
  | "docker"
  | "host_application";
export interface McpPermission {
  id: string;
  kind: McpPermissionKind;
  reason: string;
  required: boolean;
  scope?: string;
}
export interface McpCatalogDetail {
  sourceId: string;
  mcpId: string;
  version: string;
  manifestDigest: string;
  name: string;
  description: string;
  publisher: McpPublisher;
  proof: McpSourceProof;
  trustTier: McpTrustTier;
  distribution: McpDistributionContract;
  transport: McpTransportContract;
  auth: McpAuthContract;
  health: McpHealthContract;
  capabilities: McpCapability[];
  permissions: McpPermission[];
  compatibility: McpCompatibility;
  verifiedAtMs: number;
  eligibility: McpEligibility;
}

export interface McpCatalogPlanTarget {
  sourceId: string;
  mcpId: string;
  version: string;
  manifestDigest: string;
}

export type McpInstallationScope = "user";
export type McpPlanIntent =
  | {
      type: "register";
      manifest_digest: string;
      installation_scope?: McpInstallationScope;
    }
  | {
      type: "register_catalog";
      catalog: McpCatalogPlanTarget;
      installation_scope?: McpInstallationScope;
    }
  | { type: "install"; manifest_digest: string }
  | { type: "install_catalog"; catalog: McpCatalogPlanTarget }
  | { type: "update"; managed_mcp_id: string; target_version: string }
  | { type: "repair"; managed_mcp_id: string }
  | { type: "uninstall"; managed_mcp_id: string; preserve_user_data: boolean };
export type McpRollbackStrategy =
  | { type: "remove_connection_registration" }
  | { type: "staged_activation_restores_previous_version" }
  | { type: "repair_restores_verified_owned_content" }
  | { type: "uninstall_removes_owned_files"; preserve_user_data: boolean }
  | { type: "unavailable" };
export type McpPlanWarning =
  | "default_disabled"
  | "registration_only"
  | "managed_artifact_download"
  | "existing_version_retained_until_commit"
  | "removes_owned_files_only"
  | "immutable_container_image"
  | "writable_container_mount"
  | "development_source_pinned_commit_no_build";
export interface McpPlanReview {
  planId: string;
  planDigest: string;
  expiresAtMs: number;
  sourceId: string;
  catalogTarget?: McpCatalogPlanTarget;
  proof: McpSourceProof;
  trustTier: McpTrustTier;
  publisher: McpPublisher;
  mcpId: string;
  name: string;
  version: string;
  selectedManifestDigest: string;
  immutableEvidence:
    | { type: "artifact"; sha256: string; size_bytes?: number }
    | { type: "docker"; image: string; image_digest: string }
    | {
        type: "git_dev";
        repository_origin: string;
        commit: string;
        tree: McpEvidenceValue;
        materialized_digest: McpEvidenceValue;
      }
    | { type: "unavailable"; reason: McpEvidenceUnavailableReason };
  permissions: McpPermission[];
  networkOrigins: string[];
  fileEffects: {
    writesFiles: boolean;
    removesFiles: boolean;
    ownedItems: number;
  };
  hostEffects: { registrationIds: string[] };
  processEffects: {
    processRequiredForConnection: boolean;
    startsDuringConfirmation: boolean;
  };
  reversibility: { reversible: boolean; strategy: McpRollbackStrategy };
  policy: {
    outcome: "allow" | "deny" | "needs_confirmation";
    reasons: Array<{ code: string; message: string }>;
  };
  warnings: McpPlanWarning[];
  requiredConfirmations: Array<
    | { type: "policy"; reason_code: string }
    | { type: "permission"; permission_id: string }
  >;
  defaultDisabled: boolean;
  recovery: McpRecoverySuggestion;
}

export type McpEvidenceUnavailableReason =
  | "not_applicable"
  | "available_after_materialization"
  | "no_artifact_for_registration";
export type McpEvidenceValue =
  | { status: "verified"; value: string }
  | { status: "unavailable"; reason: McpEvidenceUnavailableReason };

export type McpTaskStatus =
  | "planned"
  | "awaiting_confirmation"
  | "queued"
  | "running"
  | "cancelling"
  | "verifying"
  | "activating"
  | "rolling_back"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "interrupted"
  | "recovery_required";
export interface McpTaskRef {
  taskId: string;
  operation:
    | "register"
    | "install"
    | "update"
    | "repair"
    | "uninstall"
    | "health";
  status: McpTaskStatus;
  progress: number;
  cancellable: boolean;
  revision: number;
  updatedAtMs: number;
  outcome: {
    state:
      | "pending"
      | "succeeded"
      | "failed"
      | "cancelled"
      | "interrupted"
      | "recovery_required";
    error?: {
      code: McpTaskErrorCode;
      retryable: boolean;
      correlationId: string;
      message: string;
      suggestion: McpRecoverySuggestion;
    };
    rollback:
      | "not_required"
      | "pending"
      | "in_progress"
      | "complete"
      | "incomplete";
    finalization: "pending" | "complete" | "recovery_required";
    remainingEffects: McpRemainingEffect[];
    nextAction:
      | "none"
      | "wait"
      | "cancel_when_safe"
      | "retry"
      | "resume"
      | "resolve_recovery"
      | "recreate_plan";
  };
}

export type McpTaskErrorCode =
  | "adapter_failed"
  | "verification_failed"
  | "activation_failed"
  | "rollback_failed"
  | "cancelled"
  | "interrupted"
  | "unknown";
export type McpRemainingEffect =
  | "connection_projection"
  | "managed_installation"
  | "managed_uninstall"
  | "external_resource";

export type McpRegistrationState = "absent" | "registered";
export type McpInstallationState =
  | "not_applicable"
  | "not_installed"
  | "staged"
  | "installed"
  | "update_available"
  | "repair_required"
  | "uninstall_pending";
export type McpRuntimeState =
  | "stopped"
  | "starting"
  | "running"
  | "stopping"
  | "crashed";
export type McpCredentialStatus =
  | "unconfigured"
  | "re_registration_required"
  | "trusted_state_conflict"
  | "temporarily_unavailable"
  | "ready";
export type McpHealthState =
  | "unknown"
  | "checking"
  | "healthy"
  | "degraded"
  | "unhealthy"
  | "blocked_auth"
  | "incompatible";
export type McpManagedEligibilityReason =
  | "eligible"
  | "registration_only"
  | "task_recovery_required"
  | "task_in_progress"
  | "task_interrupted"
  | "not_installed"
  | "no_update_available"
  | "runtime_unavailable"
  | "policy_denied"
  | "incompatible";
export type McpPhaseCapability = "not_available_in_this_phase" | "available";
export interface McpPhaseCapabilities {
  sessionEnablement: McpPhaseCapability;
  toolPolicy: McpPhaseCapability;
  profiles: McpPhaseCapability;
  modelSuggestions: McpPhaseCapability;
}
export interface McpManagedSummary {
  managedMcpId: string;
  mcpId: string;
  installationScope: McpInstallationScope;
  registration: McpRegistrationState;
  installation: McpInstallationState;
  runtime: McpRuntimeState;
  health: McpHealthState;
  defaultEnabled: boolean;
  revision: number;
  updatedAtMs: number;
  distributionAdapter: string;
  activeVersion: string | null;
  availableVersion: string | null;
  availableManifestDigest: string | null;
  currentTask: McpTaskRef | null;
  recoveryRequired: boolean;
  credentialStatus?: McpCredentialStatus;
  externalCapability?:
    | "docker_cli_missing"
    | "docker_daemon_unverified"
    | "docker_daemon_verified"
    | "docker_daemon_policy_denied"
    | "git_missing"
    | "git_available";
  eligibility: {
    update: boolean;
    repair: boolean;
    uninstall: boolean;
    reason: McpManagedEligibilityReason;
    updateReason: McpManagedEligibilityReason;
    repairReason: McpManagedEligibilityReason;
    uninstallReason: McpManagedEligibilityReason;
  };
  nextAction:
    | "none"
    | "enable_after_health"
    | "repair"
    | "resolve_recovery"
    | "wait_for_task"
    | "resume_task";
  phaseCapabilities: McpPhaseCapabilities;
  runtimeControl?: McpRuntimeControlCapability;
}
export interface McpRuntimeControlCapability {
  binding: string;
  canStart: boolean;
  canStop: boolean;
}
export interface McpManagedPage {
  items: McpManagedSummary[];
  nextCursor: string | null;
}
export interface McpHealthObservation {
  taskId: string;
  mode: "registration" | "runtime";
  result:
    | "healthy"
    | "unhealthy"
    | "blocked_auth"
    | "incompatible"
    | "timeout"
    | "cancelled";
  latencyMs: number;
  capabilitiesDigest: string | null;
  toolsDigest: string | null;
  checkedAtMs: number;
  detailCode:
    | "projection_consistent"
    | "projection_drift"
    | "credential_handle_missing"
    | "expected_status"
    | "unexpected_status"
    | "mcp_initialize_succeeded"
    | "mcp_initialize_failed"
    | "mcp_list_tools_succeeded"
    | "mcp_list_tools_failed"
    | "incompatible_health_contract"
    | "cleanup_failed"
    | "timeout"
    | "cancelled";
}
export interface McpHealthStatus {
  managedMcpId: string;
  state: McpHealthState;
  latest: McpHealthObservation | null;
  credentialStatus?: McpCredentialStatus;
}
export interface McpManagedDetail {
  summary: McpManagedSummary;
  distributionAdapter: string;
  activeManifestDigest: string;
  activeVersion: string;
  extensionConfigKey: string;
  projectionDigest: string;
  latestHealth: McpHealthObservation | null;
  registrationTask: McpTaskRef | null;
  supplyChain?:
    | {
        type: "docker";
        image: string;
        image_digest: string;
        adapter_version: string;
        daemon_version: string;
        rootless: boolean;
        mount_plan_digest: string;
        created_at_ms: number;
      }
    | {
        type: "git_dev";
        repository_origin: string;
        commit: string;
        git_tree_id: string;
        materialized_tree_digest: string;
        adapter_version: string;
        created_at_ms: number;
      };
}

export interface McpSourcesPolicyState {
  sources: Array<{
    sourceId: string;
    displayName: string;
    importKind: McpGovernedImportKind;
    trustTiers: McpTrustTier[];
    manifestCount: number;
    lastImportedAtMs?: number;
    cache: McpCatalogCacheMetadata;
    compatibility: { compatible: number; restricted: number; denied: number };
    recovery: McpRecoverySuggestion;
    lastRefreshDocumentDigest?: string;
    lastRefreshedAtMs?: number;
  }>;
  policy: {
    targetPlatform: string;
    targetArchitecture: string;
    developmentMode: boolean;
    dockerAllowed: boolean;
    recovery: McpRecoverySuggestion;
  };
}
export interface McpSourceRefreshResult {
  sourceId: string;
  displayName: string;
  documentDigest: string;
  manifestCount: number;
  refreshedAtMs: number;
}
export interface McpManualStdioSourcesPage {
  items: Array<{
    sourceId: string;
    displayName: string;
    publisherName: string;
    trustTier: McpTrustTier;
    compatibility: McpCompatibility;
  }>;
  provider: "available" | "operation_not_supported";
}

export type McpPlatformErrorCode =
  | "invalid_request"
  | "unsafe_url"
  | "not_found"
  | "integrity_error"
  | "policy_denied"
  | "not_implemented_for_phase"
  | "operation_not_supported"
  | "manual_stdio_provider_unavailable"
  | "remote_http_policy_unavailable"
  | "plan_stale"
  | "plan_expired"
  | "idempotency_conflict"
  | "revision_conflict"
  | "invalid_transition"
  | "repository_unavailable"
  | "projection_conflict"
  | "credential_missing"
  | "health_failed"
  | "task_not_cancellable"
  | "rollback_incomplete"
  | "adapter_incompatible"
  | "docker_unavailable"
  | "daemon_policy_denied"
  | "image_digest_mismatch"
  | "registry_auth_required"
  | "mount_permission_denied"
  | "git_unavailable"
  | "git_origin_denied"
  | "commit_unavailable"
  | "unsafe_repository_tree"
  | "development_mode_required"
  | "runtime_control_unavailable";
export interface McpPlatformErrorEnvelope {
  code: McpPlatformErrorCode;
  message: string;
  retryable: boolean;
  correlationId: string;
  details?: McpPlatformErrorDetails;
}
export type McpPlatformErrorDetails =
  | { type: "request_rejected" }
  | { type: "record_missing" }
  | { type: "integrity_validation_failed" }
  | { type: "policy_decision"; reason_codes: string[] }
  | { type: "phase_unavailable"; phase: string; operation: string }
  | { type: "plan_mismatch" }
  | { type: "plan_expired" }
  | { type: "idempotency_conflict" }
  | { type: "revision_conflict" }
  | { type: "transition_rejected" }
  | { type: "repository_temporarily_unavailable" }
  | { type: "projection_conflict" }
  | { type: "credential_missing" }
  | { type: "health_gate_failed" }
  | { type: "cancellation_deferred" }
  | { type: "recovery_required" }
  | { type: "witness_expired" }
  | { type: "witness_consumed" }
  | { type: "adapter_version_incompatible" }
  | { type: "external_capability_unavailable"; capability: string }
  | {
      type: "manual_stdio_provider_unavailable";
      recovery: McpRecoverySuggestion;
    }
  | {
      type: "remote_http_policy_unavailable";
      recovery: McpRecoverySuggestion;
    }
  | { type: "external_policy_denied"; capability: string }
  | { type: "supply_chain_mismatch"; authority: string }
  | { type: "authentication_required"; provider: string }
  | { type: "permission_grant_required"; permission: string }
  | { type: "origin_rejected"; origin_type: string }
  | { type: "immutable_commit_unavailable" }
  | { type: "repository_tree_rejected" }
  | { type: "development_mode_required" };
export type McpPlatformOutcome<T> =
  | { status: "success"; value: T }
  | { status: "error"; error: McpPlatformErrorEnvelope };
export interface McpPlatformResponse<T> {
  outcome: McpPlatformOutcome<T>;
}

export interface McpCatalogListRequest {
  query?: string;
  trustTiers?: McpTrustTier[];
  sourceIds?: string[];
  compatibility?: McpCompatibility;
  cursor?: string;
  pageSize?: number;
}
export interface McpCatalogDetailRequest {
  catalog:
    | { type: "manifest_digest"; manifest_digest: string }
    | {
        type: "catalog_ref";
        source_id: string;
        mcp_id: string;
        version: string;
      };
}
export interface McpPlanCreateRequest {
  intent: McpPlanIntent;
  idempotencyKey: string;
}
export interface McpInstallConfirmRequest {
  planId: string;
  planDigest: string;
  userDecision: "confirm" | "reject";
  idempotencyKey: string;
}
export type McpManualConnectionInput =
  | {
      type: "remote_http";
      endpoint: string;
      auth:
        | { type: "none" }
        | { type: "bearer_reference"; authReference: string };
    }
  | { type: "stdio_provider"; sourceId: string };

export interface McpEventsPage {
  events: Array<{
    eventId: number;
    taskId: string;
    taskLocalSequence: number;
    occurredAtMs: number;
    actor: string;
    eventType:
      | "task_created"
      | "task_status_changed"
      | "step_started"
      | "step_committed"
      | "recovery_interrupted"
      | "confirmation_recorded"
      | "cancellation_requested"
      | "retry_queued";
    payload: McpEventPayload;
  }>;
  nextEventId: number;
  tasks: McpTaskRef[];
}

export type McpEventPayload =
  | { type: "task_created"; status: McpTaskStatus }
  | {
      type: "task_status_changed";
      from: McpTaskStatus;
      to: McpTaskStatus;
    }
  | {
      type: "confirmation_recorded";
      from: McpTaskStatus;
      to: McpTaskStatus;
      plan_id: string;
      plan_digest: string;
    }
  | {
      type: "cancellation_requested";
      from: McpTaskStatus;
      to: McpTaskStatus;
    }
  | {
      type: "step_status_changed";
      ordinal: number;
      from: "not_started" | "started" | "committed";
      to: "not_started" | "started" | "committed";
    }
  | {
      type: "recovery_decision";
      decision:
        | { type: "resume_from_step"; ordinal: number }
        | { type: "rollback_from_step"; ordinal: number }
        | { type: "requires_manual_recovery" };
    };

type WireJsonObject = Record<string, unknown>;
type SafeWireJsonPrimitive = boolean | number | string | null;
type SafeWireJsonValue =
  | SafeWireJsonPrimitive
  | SafeWireJsonValue[]
  | { [key: string]: SafeWireJsonValue };
type ManagedDtoParser<T> = (value: unknown) => T | null;
type ManagedDtoField<T> = {
  parse: ManagedDtoParser<T>;
  optional?: boolean;
  nullable?: boolean;
};
type ManagedDtoShape = Record<string, ManagedDtoField<unknown>>;
type ParsedManagedDtoShape<TShape extends ManagedDtoShape> = {
  [TKey in keyof TShape]: TShape[TKey] extends ManagedDtoField<infer TValue>
    ? TShape[TKey]["optional"] extends true
      ? TShape[TKey]["nullable"] extends true
        ? TValue | null | undefined
        : TValue | undefined
      : TShape[TKey]["nullable"] extends true
        ? TValue | null
        : TValue
    : never;
};

const invalidWireJsonValue = Symbol("invalidWireJsonValue");

const dangerousWireKeys = new Set(["__proto__", "constructor", "prototype"]);
const responseKeys = new Set(["outcome"]);
const outcomeKeys = new Set(["status", "value", "error"]);
const successOutcomeKeys = new Set(["status", "value"]);
const errorOutcomeKeys = new Set(["status", "error"]);
const errorEnvelopeKeys = new Set([
  "code",
  "message",
  "retryable",
  "correlationId",
  "details",
]);
const knownMcpPlatformErrorCodes = new Set<McpPlatformErrorCode>([
  "invalid_request",
  "unsafe_url",
  "not_found",
  "integrity_error",
  "policy_denied",
  "not_implemented_for_phase",
  "operation_not_supported",
  "manual_stdio_provider_unavailable",
  "remote_http_policy_unavailable",
  "plan_stale",
  "plan_expired",
  "idempotency_conflict",
  "revision_conflict",
  "invalid_transition",
  "repository_unavailable",
  "projection_conflict",
  "credential_missing",
  "health_failed",
  "task_not_cancellable",
  "rollback_incomplete",
  "adapter_incompatible",
  "docker_unavailable",
  "daemon_policy_denied",
  "image_digest_mismatch",
  "registry_auth_required",
  "mount_permission_denied",
  "git_unavailable",
  "git_origin_denied",
  "commit_unavailable",
  "unsafe_repository_tree",
  "development_mode_required",
  "runtime_control_unavailable",
]);
const knownErrorDetailKeys = new Set([
  "type",
  "reason_codes",
  "operation",
  "phase",
  "capability",
  "recovery",
  "authority",
  "provider",
  "permission",
  "origin_type",
]);
const allowedMissingDetailCodes = new Set<McpPlatformErrorCode>([
  "invalid_request",
]);
const mcpRecoverySuggestionValues = new Set<McpRecoverySuggestion>([
  "none",
  "review_permissions",
  "choose_compatible_release",
  "install_required_runtime",
  "enable_development_mode",
  "restore_verified_cache",
  "retry",
  "recreate_plan",
  "resolve_recovery",
  "contact_policy_administrator",
]);
const allowedDetailTypesByCode: Record<
  McpPlatformErrorCode,
  ReadonlySet<McpPlatformErrorDetails["type"]>
> = {
  invalid_request: new Set(["request_rejected"]),
  unsafe_url: new Set(["origin_rejected"]),
  not_found: new Set(["record_missing"]),
  integrity_error: new Set(["integrity_validation_failed"]),
  policy_denied: new Set(["policy_decision"]),
  not_implemented_for_phase: new Set(["phase_unavailable"]),
  operation_not_supported: new Set(["phase_unavailable"]),
  manual_stdio_provider_unavailable: new Set([
    "manual_stdio_provider_unavailable",
  ]),
  remote_http_policy_unavailable: new Set(["remote_http_policy_unavailable"]),
  plan_stale: new Set(["plan_mismatch"]),
  plan_expired: new Set(["plan_expired"]),
  idempotency_conflict: new Set(["idempotency_conflict"]),
  revision_conflict: new Set(["revision_conflict"]),
  invalid_transition: new Set(["transition_rejected"]),
  repository_unavailable: new Set(["repository_temporarily_unavailable"]),
  projection_conflict: new Set(["projection_conflict"]),
  credential_missing: new Set(["credential_missing"]),
  health_failed: new Set(["health_gate_failed"]),
  task_not_cancellable: new Set(["cancellation_deferred"]),
  rollback_incomplete: new Set([
    "recovery_required",
    "witness_expired",
    "witness_consumed",
  ]),
  adapter_incompatible: new Set(["adapter_version_incompatible"]),
  docker_unavailable: new Set(["external_capability_unavailable"]),
  daemon_policy_denied: new Set(["external_policy_denied"]),
  image_digest_mismatch: new Set(["supply_chain_mismatch"]),
  registry_auth_required: new Set(["authentication_required"]),
  mount_permission_denied: new Set(["permission_grant_required"]),
  git_unavailable: new Set(["external_capability_unavailable"]),
  git_origin_denied: new Set(["origin_rejected"]),
  commit_unavailable: new Set(["immutable_commit_unavailable"]),
  unsafe_repository_tree: new Set(["repository_tree_rejected"]),
  development_mode_required: new Set(["development_mode_required"]),
  runtime_control_unavailable: new Set(["external_capability_unavailable"]),
};

function isPlainWireJsonObject(value: unknown): value is object {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return false;
  }
  try {
    return Object.getPrototypeOf(value) === Object.prototype;
  } catch {
    return false;
  }
}

function readWireJsonObject(
  value: unknown,
  allowedKeys: ReadonlySet<string>,
): WireJsonObject | null {
  if (!isPlainWireJsonObject(value)) {
    return null;
  }
  try {
    const record: WireJsonObject = {};
    for (const ownKey of Reflect.ownKeys(value)) {
      if (
        typeof ownKey !== "string" ||
        dangerousWireKeys.has(ownKey) ||
        !allowedKeys.has(ownKey)
      ) {
        return null;
      }
      const descriptor = Object.getOwnPropertyDescriptor(value, ownKey);
      if (
        !descriptor ||
        !descriptor.enumerable ||
        !("value" in descriptor) ||
        descriptor.get !== undefined ||
        descriptor.set !== undefined
      ) {
        return null;
      }
      record[ownKey] = descriptor.value;
    }
    return record;
  } catch {
    return null;
  }
}

function parseMcpPlatformErrorCode(
  value: unknown,
): McpPlatformErrorCode | null {
  return typeof value === "string" &&
    knownMcpPlatformErrorCodes.has(value as McpPlatformErrorCode)
    ? (value as McpPlatformErrorCode)
    : null;
}

function isStringArray(value: unknown): value is string[] {
  return (
    Array.isArray(value) && value.every((item) => typeof item === "string")
  );
}

function parseMcpRecoverySuggestion(
  value: unknown,
): McpRecoverySuggestion | null {
  return typeof value === "string" &&
    mcpRecoverySuggestionValues.has(value as McpRecoverySuggestion)
    ? (value as McpRecoverySuggestion)
    : null;
}

function parseErrorDetailsByType(
  value: WireJsonObject,
): McpPlatformErrorDetails | null {
  switch (value.type) {
    case "request_rejected":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "request_rejected" }
        : null;
    case "record_missing":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "record_missing" }
        : null;
    case "integrity_validation_failed":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "integrity_validation_failed" }
        : null;
    case "policy_decision": {
      const record = readWireJsonObject(
        value,
        new Set(["type", "reason_codes"]),
      );
      return record && isStringArray(record.reason_codes)
        ? { type: "policy_decision", reason_codes: record.reason_codes }
        : null;
    }
    case "phase_unavailable": {
      const record = readWireJsonObject(
        value,
        new Set(["type", "phase", "operation"]),
      );
      return record &&
        typeof record.phase === "string" &&
        typeof record.operation === "string"
        ? {
            type: "phase_unavailable",
            phase: record.phase,
            operation: record.operation,
          }
        : null;
    }
    case "plan_mismatch":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "plan_mismatch" }
        : null;
    case "plan_expired":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "plan_expired" }
        : null;
    case "idempotency_conflict":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "idempotency_conflict" }
        : null;
    case "revision_conflict":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "revision_conflict" }
        : null;
    case "transition_rejected":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "transition_rejected" }
        : null;
    case "repository_temporarily_unavailable":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "repository_temporarily_unavailable" }
        : null;
    case "projection_conflict":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "projection_conflict" }
        : null;
    case "credential_missing":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "credential_missing" }
        : null;
    case "health_gate_failed":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "health_gate_failed" }
        : null;
    case "cancellation_deferred":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "cancellation_deferred" }
        : null;
    case "recovery_required":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "recovery_required" }
        : null;
    case "witness_expired":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "witness_expired" }
        : null;
    case "witness_consumed":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "witness_consumed" }
        : null;
    case "adapter_version_incompatible":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "adapter_version_incompatible" }
        : null;
    case "external_capability_unavailable": {
      const record = readWireJsonObject(value, new Set(["type", "capability"]));
      return record && typeof record.capability === "string"
        ? {
            type: "external_capability_unavailable",
            capability: record.capability,
          }
        : null;
    }
    case "manual_stdio_provider_unavailable": {
      const record = readWireJsonObject(value, new Set(["type", "recovery"]));
      const recovery = record
        ? parseMcpRecoverySuggestion(record.recovery)
        : null;
      return recovery
        ? { type: "manual_stdio_provider_unavailable", recovery }
        : null;
    }
    case "remote_http_policy_unavailable": {
      const record = readWireJsonObject(value, new Set(["type", "recovery"]));
      const recovery = record
        ? parseMcpRecoverySuggestion(record.recovery)
        : null;
      return recovery
        ? { type: "remote_http_policy_unavailable", recovery }
        : null;
    }
    case "external_policy_denied": {
      const record = readWireJsonObject(value, new Set(["type", "capability"]));
      return record && typeof record.capability === "string"
        ? { type: "external_policy_denied", capability: record.capability }
        : null;
    }
    case "supply_chain_mismatch": {
      const record = readWireJsonObject(value, new Set(["type", "authority"]));
      return record && typeof record.authority === "string"
        ? { type: "supply_chain_mismatch", authority: record.authority }
        : null;
    }
    case "authentication_required": {
      const record = readWireJsonObject(value, new Set(["type", "provider"]));
      return record && typeof record.provider === "string"
        ? { type: "authentication_required", provider: record.provider }
        : null;
    }
    case "permission_grant_required": {
      const record = readWireJsonObject(value, new Set(["type", "permission"]));
      return record && typeof record.permission === "string"
        ? { type: "permission_grant_required", permission: record.permission }
        : null;
    }
    case "origin_rejected": {
      const record = readWireJsonObject(
        value,
        new Set(["type", "origin_type"]),
      );
      return record && typeof record.origin_type === "string"
        ? { type: "origin_rejected", origin_type: record.origin_type }
        : null;
    }
    case "immutable_commit_unavailable":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "immutable_commit_unavailable" }
        : null;
    case "repository_tree_rejected":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "repository_tree_rejected" }
        : null;
    case "development_mode_required":
      return readWireJsonObject(value, new Set(["type"]))
        ? { type: "development_mode_required" }
        : null;
    default:
      return null;
  }
}

function parseMcpPlatformErrorDetails(
  code: McpPlatformErrorCode,
  value: unknown,
): McpPlatformErrorDetails | undefined | null {
  if (value === undefined) {
    return allowedMissingDetailCodes.has(code) ? undefined : null;
  }
  const record = readWireJsonObject(value, knownErrorDetailKeys);
  if (!record || typeof record.type !== "string") {
    return null;
  }
  const parsed = parseErrorDetailsByType(record);
  return parsed && allowedDetailTypesByCode[code].has(parsed.type)
    ? parsed
    : null;
}

function parseMcpPlatformErrorEnvelope(
  value: unknown,
): McpPlatformErrorEnvelope | null {
  const record = readWireJsonObject(value, errorEnvelopeKeys);
  if (!record) {
    return null;
  }
  const code = parseMcpPlatformErrorCode(record.code);
  const details = code
    ? parseMcpPlatformErrorDetails(code, record.details)
    : null;
  if (
    code === null ||
    typeof record.message !== "string" ||
    typeof record.retryable !== "boolean" ||
    typeof record.correlationId !== "string" ||
    details === null
  ) {
    return null;
  }
  return {
    code,
    message: record.message,
    retryable: record.retryable,
    correlationId: record.correlationId,
    details,
  };
}

const installationScopeValues = new Set<McpInstallationScope>(["user"]);
const registrationStateValues = new Set<McpRegistrationState>([
  "absent",
  "registered",
]);
const installationStateValues = new Set<McpInstallationState>([
  "not_applicable",
  "not_installed",
  "staged",
  "installed",
  "update_available",
  "repair_required",
  "uninstall_pending",
]);
const runtimeStateValues = new Set<McpRuntimeState>([
  "stopped",
  "starting",
  "running",
  "stopping",
  "crashed",
]);
const healthStateValues = new Set<McpHealthState>([
  "unknown",
  "checking",
  "healthy",
  "degraded",
  "unhealthy",
  "blocked_auth",
  "incompatible",
]);
const credentialStatusValues = new Set<McpCredentialStatus>([
  "unconfigured",
  "re_registration_required",
  "trusted_state_conflict",
  "temporarily_unavailable",
  "ready",
]);
const healthCheckModeValues = new Set<McpHealthObservation["mode"]>([
  "registration",
  "runtime",
]);
const healthResultValues = new Set<McpHealthObservation["result"]>([
  "healthy",
  "unhealthy",
  "blocked_auth",
  "incompatible",
  "timeout",
  "cancelled",
]);
const healthDetailCodeValues = new Set<McpHealthObservation["detailCode"]>([
  "projection_consistent",
  "projection_drift",
  "credential_handle_missing",
  "expected_status",
  "unexpected_status",
  "mcp_initialize_succeeded",
  "mcp_initialize_failed",
  "mcp_list_tools_succeeded",
  "mcp_list_tools_failed",
  "incompatible_health_contract",
  "cleanup_failed",
  "timeout",
  "cancelled",
]);
const taskOperationValues = new Set<McpTaskRef["operation"]>([
  "register",
  "install",
  "update",
  "repair",
  "uninstall",
  "health",
]);
const taskStatusValues = new Set<McpTaskStatus>([
  "planned",
  "awaiting_confirmation",
  "queued",
  "running",
  "cancelling",
  "verifying",
  "activating",
  "rolling_back",
  "succeeded",
  "failed",
  "cancelled",
  "interrupted",
  "recovery_required",
]);
const taskOutcomeStateValues = new Set<McpTaskRef["outcome"]["state"]>([
  "pending",
  "succeeded",
  "failed",
  "cancelled",
  "interrupted",
  "recovery_required",
]);
const taskErrorCodeValues = new Set<McpTaskErrorCode>([
  "adapter_failed",
  "verification_failed",
  "activation_failed",
  "rollback_failed",
  "cancelled",
  "interrupted",
  "unknown",
]);
const taskRollbackValues = new Set<McpTaskRef["outcome"]["rollback"]>([
  "not_required",
  "pending",
  "in_progress",
  "complete",
  "incomplete",
]);
const taskFinalizationValues = new Set<McpTaskRef["outcome"]["finalization"]>([
  "pending",
  "complete",
  "recovery_required",
]);
const remainingEffectValues = new Set<McpRemainingEffect>([
  "connection_projection",
  "managed_installation",
  "managed_uninstall",
  "external_resource",
]);
const taskNextActionValues = new Set<McpTaskRef["outcome"]["nextAction"]>([
  "none",
  "wait",
  "cancel_when_safe",
  "retry",
  "resume",
  "resolve_recovery",
  "recreate_plan",
]);
const externalCapabilityValues = new Set<
  NonNullable<McpManagedSummary["externalCapability"]>
>([
  "docker_cli_missing",
  "docker_daemon_unverified",
  "docker_daemon_verified",
  "docker_daemon_policy_denied",
  "git_missing",
  "git_available",
]);
const managedEligibilityReasonValues = new Set<McpManagedEligibilityReason>([
  "eligible",
  "registration_only",
  "task_recovery_required",
  "task_in_progress",
  "task_interrupted",
  "not_installed",
  "no_update_available",
  "runtime_unavailable",
  "policy_denied",
  "incompatible",
]);
const managedNextActionValues = new Set<McpManagedSummary["nextAction"]>([
  "none",
  "enable_after_health",
  "repair",
  "resolve_recovery",
  "wait_for_task",
  "resume_task",
]);
const phaseCapabilityValues = new Set<McpPhaseCapability>([
  "not_available_in_this_phase",
  "available",
]);

function hasExactKeys(
  value: Record<string, unknown>,
  keys: ReadonlySet<string>,
): boolean {
  const actualKeys = Object.keys(value);
  return (
    actualKeys.length === keys.size && actualKeys.every((key) => keys.has(key))
  );
}

function defineManagedDtoShape<TShape extends ManagedDtoShape>(
  shape: TShape,
): TShape {
  return shape;
}

function createManagedDtoParser<TShape extends ManagedDtoShape>(
  shape: TShape,
): ManagedDtoParser<ParsedManagedDtoShape<TShape>> {
  const allowedKeys = new Set(Object.keys(shape));
  return (value) => {
    const record = readWireJsonObject(value, allowedKeys);
    if (!record) {
      return null;
    }

    const parsed: Partial<ParsedManagedDtoShape<TShape>> = {};
    for (const key of Object.keys(shape) as Array<keyof TShape>) {
      const field = shape[key];
      if (!Object.prototype.hasOwnProperty.call(record, key)) {
        if (field.optional) {
          continue;
        }
        return null;
      }
      const rawValue = record[key as string];
      if (rawValue === null) {
        if (!field.nullable) {
          return null;
        }
        parsed[key] = null as ParsedManagedDtoShape<TShape>[typeof key];
        continue;
      }
      const parsedValue = field.parse(rawValue);
      if (parsedValue === null) {
        return null;
      }
      parsed[key] = parsedValue as ParsedManagedDtoShape<TShape>[typeof key];
    }

    return parsed as ParsedManagedDtoShape<TShape>;
  };
}

function createEnumParser<T extends string>(
  allowedValues: ReadonlySet<T>,
): ManagedDtoParser<T> {
  return (value) =>
    typeof value === "string" && allowedValues.has(value as T)
      ? (value as T)
      : null;
}

function parseBooleanValue(value: unknown): boolean | null {
  return typeof value === "boolean" ? value : null;
}

function parseStringValue(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function parseSafeIntegerValue(value: unknown): number | null {
  return typeof value === "number" && Number.isSafeInteger(value)
    ? value
    : null;
}

function createBoundedIntegerParser(
  minimum: number,
  maximum: number,
): ManagedDtoParser<number> {
  return (value) => {
    const parsed = parseSafeIntegerValue(value);
    return parsed !== null && parsed >= minimum && parsed <= maximum
      ? parsed
      : null;
  };
}

function createArrayParser<T>(
  itemParser: ManagedDtoParser<T>,
): ManagedDtoParser<T[]> {
  return (value) => {
    if (!Array.isArray(value)) {
      return null;
    }
    const parsedItems: T[] = [];
    for (const item of value) {
      const parsedItem = itemParser(item);
      if (parsedItem === null) {
        return null;
      }
      parsedItems.push(parsedItem);
    }
    return parsedItems;
  };
}

function isPlainWireJsonArray(value: unknown): value is unknown[] {
  if (!Array.isArray(value)) {
    return false;
  }
  try {
    return Object.getPrototypeOf(value) === Array.prototype;
  } catch {
    return false;
  }
}

function isArrayIndexKey(key: string): boolean {
  return /^(?:0|[1-9]\d*)$/.test(key);
}

function sanitizeWireJsonValue(
  value: unknown,
  seen = new Set<object>(),
): SafeWireJsonValue | typeof invalidWireJsonValue {
  if (value === null) {
    return null;
  }
  switch (typeof value) {
    case "boolean":
    case "string":
      return value;
    case "number":
      return Number.isFinite(value) ? value : invalidWireJsonValue;
    case "undefined":
    case "bigint":
    case "function":
    case "symbol":
      return invalidWireJsonValue;
    case "object":
      break;
  }

  if (seen.has(value)) {
    return invalidWireJsonValue;
  }
  seen.add(value);
  try {
    if (isPlainWireJsonArray(value)) {
      return sanitizeWireJsonArray(value, seen);
    }
    if (!isPlainWireJsonObject(value)) {
      return invalidWireJsonValue;
    }
    return sanitizeWireJsonRecord(value, seen);
  } finally {
    seen.delete(value);
  }
}

function sanitizeWireJsonArray(
  value: unknown[],
  seen: Set<object>,
): SafeWireJsonValue[] | typeof invalidWireJsonValue {
  try {
    const lengthDescriptor = Object.getOwnPropertyDescriptor(value, "length");
    if (
      !lengthDescriptor ||
      !("value" in lengthDescriptor) ||
      lengthDescriptor.get !== undefined ||
      lengthDescriptor.set !== undefined ||
      lengthDescriptor.enumerable ||
      !Number.isSafeInteger(lengthDescriptor.value) ||
      lengthDescriptor.value < 0
    ) {
      return invalidWireJsonValue;
    }

    const length = lengthDescriptor.value;
    const sanitized: SafeWireJsonValue[] = [];
    for (const ownKey of Reflect.ownKeys(value)) {
      if (typeof ownKey !== "string") {
        return invalidWireJsonValue;
      }
      if (ownKey === "length") {
        continue;
      }
      if (dangerousWireKeys.has(ownKey) || !isArrayIndexKey(ownKey)) {
        return invalidWireJsonValue;
      }
      const index = Number(ownKey);
      if (!Number.isSafeInteger(index) || index < 0 || index >= length) {
        return invalidWireJsonValue;
      }
    }

    for (let index = 0; index < length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (
        !descriptor ||
        !descriptor.enumerable ||
        !("value" in descriptor) ||
        descriptor.get !== undefined ||
        descriptor.set !== undefined
      ) {
        return invalidWireJsonValue;
      }
      const sanitizedItem = sanitizeWireJsonValue(descriptor.value, seen);
      if (sanitizedItem === invalidWireJsonValue) {
        return invalidWireJsonValue;
      }
      sanitized.push(sanitizedItem);
    }
    return sanitized;
  } catch {
    return invalidWireJsonValue;
  }
}

function sanitizeWireJsonRecord(
  value: object,
  seen: Set<object>,
): { [key: string]: SafeWireJsonValue } | typeof invalidWireJsonValue {
  try {
    const sanitized: { [key: string]: SafeWireJsonValue } = {};
    for (const ownKey of Reflect.ownKeys(value)) {
      if (typeof ownKey !== "string" || dangerousWireKeys.has(ownKey)) {
        return invalidWireJsonValue;
      }
      const descriptor = Object.getOwnPropertyDescriptor(value, ownKey);
      if (
        !descriptor ||
        !descriptor.enumerable ||
        !("value" in descriptor) ||
        descriptor.get !== undefined ||
        descriptor.set !== undefined
      ) {
        return invalidWireJsonValue;
      }
      const sanitizedValue = sanitizeWireJsonValue(descriptor.value, seen);
      if (sanitizedValue === invalidWireJsonValue) {
        return invalidWireJsonValue;
      }
      sanitized[ownKey] = sanitizedValue;
    }
    return sanitized;
  } catch {
    return invalidWireJsonValue;
  }
}

const parsePhaseCapabilities = createManagedDtoParser(
  defineManagedDtoShape({
    sessionEnablement: { parse: createEnumParser(phaseCapabilityValues) },
    toolPolicy: { parse: createEnumParser(phaseCapabilityValues) },
    profiles: { parse: createEnumParser(phaseCapabilityValues) },
    modelSuggestions: { parse: createEnumParser(phaseCapabilityValues) },
  }),
);

const parseRuntimeControlCapability = createManagedDtoParser(
  defineManagedDtoShape({
    binding: { parse: parseStringValue },
    canStart: { parse: parseBooleanValue },
    canStop: { parse: parseBooleanValue },
  }),
);

const parseTaskClosedError = createManagedDtoParser(
  defineManagedDtoShape({
    code: { parse: createEnumParser(taskErrorCodeValues) },
    retryable: { parse: parseBooleanValue },
    correlationId: { parse: parseStringValue },
    message: { parse: parseStringValue },
    suggestion: { parse: parseMcpRecoverySuggestion },
  }),
);

const parseTaskOutcomeSummary = createManagedDtoParser(
  defineManagedDtoShape({
    state: { parse: createEnumParser(taskOutcomeStateValues) },
    error: { parse: parseTaskClosedError, optional: true },
    rollback: { parse: createEnumParser(taskRollbackValues) },
    finalization: { parse: createEnumParser(taskFinalizationValues) },
    remainingEffects: {
      parse: createArrayParser(createEnumParser(remainingEffectValues)),
    },
    nextAction: { parse: createEnumParser(taskNextActionValues) },
  }),
);

const parseTaskRef = createManagedDtoParser(
  defineManagedDtoShape({
    taskId: { parse: parseStringValue },
    operation: { parse: createEnumParser(taskOperationValues) },
    status: { parse: createEnumParser(taskStatusValues) },
    progress: { parse: createBoundedIntegerParser(0, 255) },
    cancellable: { parse: parseBooleanValue },
    revision: { parse: parseSafeIntegerValue },
    updatedAtMs: { parse: parseSafeIntegerValue },
    outcome: { parse: parseTaskOutcomeSummary },
  }),
);

const parseManagedEligibility = createManagedDtoParser(
  defineManagedDtoShape({
    update: { parse: parseBooleanValue },
    repair: { parse: parseBooleanValue },
    uninstall: { parse: parseBooleanValue },
    reason: { parse: createEnumParser(managedEligibilityReasonValues) },
    updateReason: { parse: createEnumParser(managedEligibilityReasonValues) },
    repairReason: { parse: createEnumParser(managedEligibilityReasonValues) },
    uninstallReason: {
      parse: createEnumParser(managedEligibilityReasonValues),
    },
  }),
);

const parseHealthObservation = createManagedDtoParser(
  defineManagedDtoShape({
    taskId: { parse: parseStringValue },
    mode: { parse: createEnumParser(healthCheckModeValues) },
    result: { parse: createEnumParser(healthResultValues) },
    latencyMs: { parse: parseSafeIntegerValue },
    capabilitiesDigest: { parse: parseStringValue, nullable: true },
    toolsDigest: { parse: parseStringValue, nullable: true },
    checkedAtMs: { parse: parseSafeIntegerValue },
    detailCode: { parse: createEnumParser(healthDetailCodeValues) },
  }),
);

const parseHealthStatus = createManagedDtoParser(
  defineManagedDtoShape({
    managedMcpId: { parse: parseStringValue },
    state: { parse: createEnumParser(healthStateValues) },
    latest: { parse: parseHealthObservation, nullable: true },
    credentialStatus: {
      parse: createEnumParser(credentialStatusValues),
      optional: true,
    },
  }),
);

const parseDockerSupplyChain = createManagedDtoParser(
  defineManagedDtoShape({
    type: { parse: createEnumParser(new Set(["docker"] as const)) },
    image: { parse: parseStringValue },
    image_digest: { parse: parseStringValue },
    adapter_version: { parse: parseStringValue },
    daemon_version: { parse: parseStringValue },
    rootless: { parse: parseBooleanValue },
    mount_plan_digest: { parse: parseStringValue },
    created_at_ms: { parse: parseSafeIntegerValue },
  }),
);

const parseGitDevSupplyChain = createManagedDtoParser(
  defineManagedDtoShape({
    type: { parse: createEnumParser(new Set(["git_dev"] as const)) },
    repository_origin: { parse: parseStringValue },
    commit: { parse: parseStringValue },
    git_tree_id: { parse: parseStringValue },
    materialized_tree_digest: { parse: parseStringValue },
    adapter_version: { parse: parseStringValue },
    created_at_ms: { parse: parseSafeIntegerValue },
  }),
);

function parseManagedSupplyChain(
  value: unknown,
): NonNullable<McpManagedDetail["supplyChain"]> | null {
  const record = readWireJsonObject(
    value,
    new Set([
      "type",
      "image",
      "image_digest",
      "adapter_version",
      "daemon_version",
      "rootless",
      "mount_plan_digest",
      "created_at_ms",
      "repository_origin",
      "commit",
      "git_tree_id",
      "materialized_tree_digest",
    ]),
  );
  if (!record || typeof record.type !== "string") {
    return null;
  }
  if (record.type === "docker") {
    return parseDockerSupplyChain(value);
  }
  if (record.type === "git_dev") {
    return parseGitDevSupplyChain(value);
  }
  return null;
}

const parseManagedSummary = createManagedDtoParser(
  defineManagedDtoShape({
    managedMcpId: { parse: parseStringValue },
    mcpId: { parse: parseStringValue },
    installationScope: { parse: createEnumParser(installationScopeValues) },
    registration: { parse: createEnumParser(registrationStateValues) },
    installation: { parse: createEnumParser(installationStateValues) },
    runtime: { parse: createEnumParser(runtimeStateValues) },
    health: { parse: createEnumParser(healthStateValues) },
    defaultEnabled: { parse: parseBooleanValue },
    revision: { parse: parseSafeIntegerValue },
    updatedAtMs: { parse: parseSafeIntegerValue },
    distributionAdapter: { parse: parseStringValue },
    activeVersion: { parse: parseStringValue, nullable: true },
    availableVersion: { parse: parseStringValue, nullable: true },
    availableManifestDigest: { parse: parseStringValue, nullable: true },
    currentTask: { parse: parseTaskRef, nullable: true },
    recoveryRequired: { parse: parseBooleanValue },
    credentialStatus: {
      parse: createEnumParser(credentialStatusValues),
      optional: true,
    },
    externalCapability: {
      parse: createEnumParser(externalCapabilityValues),
      optional: true,
    },
    eligibility: { parse: parseManagedEligibility },
    nextAction: { parse: createEnumParser(managedNextActionValues) },
    phaseCapabilities: { parse: parsePhaseCapabilities },
    runtimeControl: { parse: parseRuntimeControlCapability, optional: true },
  }),
);

const parseManagedPage = createManagedDtoParser(
  defineManagedDtoShape({
    items: { parse: createArrayParser(parseManagedSummary) },
    nextCursor: { parse: parseStringValue, nullable: true },
  }),
);

const parseManagedDetail = createManagedDtoParser(
  defineManagedDtoShape({
    summary: { parse: parseManagedSummary },
    distributionAdapter: { parse: parseStringValue },
    activeManifestDigest: { parse: parseStringValue },
    activeVersion: { parse: parseStringValue },
    extensionConfigKey: { parse: parseStringValue },
    projectionDigest: { parse: parseStringValue },
    latestHealth: { parse: parseHealthObservation, nullable: true },
    registrationTask: { parse: parseTaskRef, nullable: true },
    supplyChain: { parse: parseManagedSupplyChain, optional: true },
  }),
);

const successPayloadParsers: Partial<
  Record<string, (value: unknown) => unknown | null>
> = {
  "goose.mcpList_unstable": parseManagedPage,
  "goose.mcpGet_unstable": parseManagedDetail,
  "goose.mcpSetDefaultEnabled_unstable": parseManagedSummary,
  "goose.mcpRuntimeControl_unstable": parseManagedSummary,
  "goose.mcpHealthGet_unstable": parseHealthStatus,
};

function parseResponse<T>(
  raw: unknown,
  method: string,
): McpPlatformResponse<T> {
  const response = readWireJsonObject(raw, responseKeys);
  if (!response) {
    throw new Error(`${method} returned an invalid MCP Platform outcome`);
  }
  const baseOutcome = readWireJsonObject(response.outcome, outcomeKeys);
  if (!baseOutcome || typeof baseOutcome.status !== "string") {
    throw new Error(`${method} returned an invalid MCP Platform outcome`);
  }
  if (baseOutcome.status === "success") {
    const outcome = readWireJsonObject(response.outcome, successOutcomeKeys);
    if (!outcome || !hasExactKeys(outcome, successOutcomeKeys)) {
      throw new Error(`${method} returned success without a value`);
    }
    const sanitizedValue = sanitizeWireJsonValue(outcome.value);
    if (sanitizedValue === invalidWireJsonValue) {
      throw new Error(
        `${method} returned an invalid MCP Platform success payload`,
      );
    }
    const parseSuccessPayload = successPayloadParsers[method];
    const value = parseSuccessPayload
      ? parseSuccessPayload(sanitizedValue)
      : sanitizedValue;
    if (parseSuccessPayload && value === null) {
      throw new Error(
        `${method} returned an invalid MCP Platform success payload`,
      );
    }
    return {
      outcome: {
        status: "success",
        value: value as T,
      },
    };
  }

  if (baseOutcome.status !== "error") {
    throw new Error(`${method} returned an invalid MCP Platform outcome`);
  }
  const outcome = readWireJsonObject(response.outcome, errorOutcomeKeys);
  if (!outcome || !hasExactKeys(outcome, errorOutcomeKeys)) {
    throw new Error(`${method} returned an invalid MCP Platform error`);
  }
  const error = outcome ? parseMcpPlatformErrorEnvelope(outcome.error) : null;
  if (!error) {
    throw new Error(`${method} returned an invalid MCP Platform error`);
  }
  return {
    outcome: {
      status: "error",
      error,
    },
  };
}

export class McpPlatformClient {
  constructor(private readonly conn: ExtMethodProvider) {}

  private async request<T>(
    method: string,
    params: object,
  ): Promise<McpPlatformResponse<T>> {
    const requestParams = Object.fromEntries(Object.entries(params));
    return parseResponse<T>(
      await this.conn.extMethod(method, requestParams),
      method,
    );
  }

  mcpCatalogList_unstable(params: McpCatalogListRequest) {
    return this.request<McpCatalogPage>(
      "goose.mcpCatalogList_unstable",
      params,
    );
  }
  mcpCatalogDetail_unstable(params: McpCatalogDetailRequest) {
    return this.request<McpCatalogDetail>(
      "goose.mcpCatalogDetail_unstable",
      params,
    );
  }
  mcpSourcesPolicyGet_unstable() {
    return this.request<McpSourcesPolicyState>(
      "goose.mcpSourcesPolicyGet_unstable",
      {},
    );
  }
  mcpSourceRefresh_unstable(params: { sourceId: string }) {
    return this.request<McpSourceRefreshResult>(
      "goose.mcpSourceRefresh_unstable",
      params,
    );
  }
  mcpManualStdioSourcesList_unstable() {
    return this.request<McpManualStdioSourcesPage>(
      "goose.mcpManualStdioSourcesList_unstable",
      {},
    );
  }
  mcpManualPlanCreate_unstable(params: {
    connection: McpManualConnectionInput;
    idempotencyKey: string;
  }) {
    return this.request<McpPlanReview>(
      "goose.mcpManualPlanCreate_unstable",
      params,
    );
  }
  mcpPlanCreate_unstable(params: McpPlanCreateRequest) {
    return this.request<McpPlanReview>("goose.mcpPlanCreate_unstable", params);
  }
  mcpInstallConfirm_unstable(params: McpInstallConfirmRequest) {
    return this.request<McpTaskRef>("goose.mcpInstallConfirm_unstable", params);
  }
  mcpTaskGet_unstable(params: { taskId: string }) {
    return this.request<McpTaskRef>("goose.mcpTaskGet_unstable", params);
  }
  mcpTaskCancel_unstable(params: { taskId: string; expectedRevision: number }) {
    return this.request<McpTaskRef>("goose.mcpTaskCancel_unstable", params);
  }
  mcpTaskRetry_unstable(params: {
    taskId: string;
    expectedRevision: number;
    idempotencyKey: string;
  }) {
    return this.request<McpTaskRef>("goose.mcpTaskRetry_unstable", params);
  }
  mcpEventsResume_unstable(params: {
    afterEventId?: number;
    limit?: number;
    taskIds?: string[];
  }) {
    return this.request<McpEventsPage>(
      "goose.mcpEventsResume_unstable",
      params,
    );
  }
  mcpList_unstable(params: {
    cursor?: string;
    pageSize?: number;
    registration?: McpRegistrationState;
    installation?: McpInstallationState;
    runtime?: McpRuntimeState;
    health?: McpHealthState;
    defaultEnabled?: boolean;
  }) {
    return this.request<McpManagedPage>("goose.mcpList_unstable", params);
  }
  mcpGet_unstable(params: { managedMcpId: string }) {
    return this.request<McpManagedDetail>("goose.mcpGet_unstable", params);
  }
  mcpHealthRun_unstable(params: {
    managedMcpId: string;
    mode: "registration" | "runtime";
    idempotencyKey: string;
  }) {
    return this.request<McpTaskRef>("goose.mcpHealthRun_unstable", params);
  }
  mcpHealthGet_unstable(params: { managedMcpId: string }) {
    return this.request<McpHealthStatus>("goose.mcpHealthGet_unstable", params);
  }
  mcpSetDefaultEnabled_unstable(params: {
    managedMcpId: string;
    enabled: boolean;
    expectedRevision: number;
  }) {
    return this.request<McpManagedSummary>(
      "goose.mcpSetDefaultEnabled_unstable",
      params,
    );
  }
  mcpRuntimeControl_unstable(params: {
    managedMcpId: string;
    expectedRevision: number;
    action: "start" | "stop";
    runtimeBinding: string;
  }) {
    return this.request<McpManagedSummary>(
      "goose.mcpRuntimeControl_unstable",
      params,
    );
  }
}
