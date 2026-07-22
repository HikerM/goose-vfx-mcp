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

export type McpInstallationScope = "user";
export type McpPlanIntent =
  | {
      type: "register";
      manifest_digest: string;
      installation_scope?: McpInstallationScope;
    }
  | { type: "install"; manifest_digest: string }
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
  activeVersion?: string;
  availableVersion?: string;
  availableManifestDigest?: string;
  currentTask?: McpTaskRef;
  recoveryRequired: boolean;
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
  phaseCapabilities: {
    sessionEnablement: "not_available_in_this_phase";
    toolPolicy: "not_available_in_this_phase";
    profiles: "not_available_in_this_phase";
    modelSuggestions: "not_available_in_this_phase";
  };
}
export interface McpManagedPage {
  items: McpManagedSummary[];
  nextCursor?: string;
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
  capabilitiesDigest?: string;
  toolsDigest?: string;
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
  latest?: McpHealthObservation;
}
export interface McpManagedDetail {
  summary: McpManagedSummary;
  distributionAdapter: string;
  activeManifestDigest: string;
  activeVersion: string;
  extensionConfigKey: string;
  projectionDigest: string;
  latestHealth?: McpHealthObservation;
  registrationTask?: McpTaskRef;
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
    trustTiers: McpTrustTier[];
    manifestCount: number;
    cache: McpCatalogCacheMetadata;
    compatibility: { compatible: number; restricted: number; denied: number };
    recovery: McpRecoverySuggestion;
  }>;
  policy: {
    targetPlatform: string;
    targetArchitecture: string;
    developmentMode: boolean;
    dockerAllowed: boolean;
    recovery: McpRecoverySuggestion;
  };
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
  | "development_mode_required";
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

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function parseResponse<T>(
  raw: Record<string, unknown>,
  method: string,
): McpPlatformResponse<T> {
  const outcome = raw.outcome;
  if (
    !isRecord(outcome) ||
    (outcome.status !== "success" && outcome.status !== "error")
  ) {
    throw new Error(`${method} returned an invalid MCP Platform outcome`);
  }
  if (outcome.status === "success" && !("value" in outcome)) {
    throw new Error(`${method} returned success without a value`);
  }
  if (outcome.status === "error") {
    const error = outcome.error;
    if (
      !isRecord(error) ||
      typeof error.code !== "string" ||
      typeof error.message !== "string" ||
      typeof error.retryable !== "boolean" ||
      typeof error.correlationId !== "string"
    ) {
      throw new Error(`${method} returned an invalid MCP Platform error`);
    }
  }
  return raw as unknown as McpPlatformResponse<T>;
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
}
