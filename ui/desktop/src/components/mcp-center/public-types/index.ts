export type PublicTaskReference = {
  readonly kind: 'mcp_task';
  readonly opaqueId: string;
};

export type PublicTaskEventReference = {
  readonly kind: 'mcp_task_event';
  readonly opaqueId: string;
};

export type PublicTaskStatus =
  | 'planned'
  | 'awaiting_confirmation'
  | 'queued'
  | 'running'
  | 'cancelling'
  | 'verifying'
  | 'activating'
  | 'rolling_back'
  | 'succeeded'
  | 'failed'
  | 'cancelled'
  | 'interrupted'
  | 'recovery_required'
  | 'unknown';

export type PublicTaskDisplayState =
  | 'active'
  | 'paused'
  | 'recovery'
  | 'succeeded'
  | 'failed'
  | 'cancelled'
  | 'unknown';

export type PublicTaskOperation =
  | 'register'
  | 'install'
  | 'update'
  | 'repair'
  | 'uninstall'
  | 'health'
  | 'unknown';

export type PublicTaskOutcomeState =
  | 'pending'
  | 'succeeded'
  | 'failed'
  | 'cancelled'
  | 'interrupted'
  | 'recovery_required'
  | 'unknown';

export type PublicTaskRollbackState =
  | 'not_required'
  | 'pending'
  | 'in_progress'
  | 'complete'
  | 'incomplete'
  | 'unknown';

export type PublicTaskFinalizationState =
  | 'pending'
  | 'complete'
  | 'recovery_required'
  | 'unknown';

export type PublicTaskNextAction =
  | 'none'
  | 'wait'
  | 'cancel_when_safe'
  | 'retry'
  | 'recovery'
  | 'unknown';

export type PublicTaskRemainingEffect =
  | 'connection_projection'
  | 'managed_installation'
  | 'managed_uninstall'
  | 'external_resource'
  | 'unknown';

export type PublicTaskCapabilityState =
  | 'available'
  | 'unavailable'
  | 'paused'
  | 'recovery'
  | 'unknown';

export type McpCenterSafeErrorCode =
  | 'invalid_request'
  | 'unsafe_url'
  | 'not_found'
  | 'integrity_error'
  | 'policy_denied'
  | 'not_implemented_for_phase'
  | 'operation_not_supported'
  | 'manual_stdio_provider_unavailable'
  | 'remote_http_policy_unavailable'
  | 'plan_stale'
  | 'plan_expired'
  | 'idempotency_conflict'
  | 'revision_conflict'
  | 'invalid_transition'
  | 'repository_unavailable'
  | 'projection_conflict'
  | 'credential_missing'
  | 'health_failed'
  | 'task_not_cancellable'
  | 'rollback_incomplete'
  | 'adapter_incompatible'
  | 'docker_unavailable'
  | 'daemon_policy_denied'
  | 'image_digest_mismatch'
  | 'registry_auth_required'
  | 'mount_permission_denied'
  | 'git_unavailable'
  | 'git_origin_denied'
  | 'commit_unavailable'
  | 'unsafe_repository_tree'
  | 'development_mode_required'
  | 'adapter_failed'
  | 'verification_failed'
  | 'activation_failed'
  | 'rollback_failed'
  | 'cancelled'
  | 'interrupted'
  | 'connection_unavailable'
  | 'monitor_paused'
  | 'unknown';

export type McpCenterSafeErrorUserCopyKey =
  | 'mcpCenter.safeError.generic'
  | `mcpCenter.safeError.${McpCenterSafeErrorCode}`;

export type McpCenterSafeError = {
  code: McpCenterSafeErrorCode;
  retryable: boolean;
  supportRef: string;
  userCopyKey: McpCenterSafeErrorUserCopyKey;
};

export type PublicTaskCapabilities = {
  cancel: PublicTaskCapabilityState;
  retry: PublicTaskCapabilityState;
  review: PublicTaskCapabilityState;
};

export type PublicTaskOutcome = {
  state: PublicTaskOutcomeState;
  rollback: PublicTaskRollbackState;
  finalization: PublicTaskFinalizationState;
  remainingEffects: PublicTaskRemainingEffect[];
  nextAction: PublicTaskNextAction;
  error?: McpCenterSafeError;
};

export type PublicTask = {
  reference: PublicTaskReference;
  operation: PublicTaskOperation;
  status: PublicTaskStatus;
  displayState: PublicTaskDisplayState;
  progressPercent: number;
  revision: number;
  updatedAtMs: number;
  capabilities: PublicTaskCapabilities;
  outcome: PublicTaskOutcome;
};

export type PublicTaskMutationTarget = Pick<PublicTask, 'reference' | 'revision'>;

export type PublicTaskEventKind =
  | 'created'
  | 'status_changed'
  | 'confirmation'
  | 'cancellation_requested'
  | 'step_status_changed'
  | 'recovery_decision'
  | 'unknown';

export type PublicTaskEventDisplayState =
  | 'active'
  | 'paused'
  | 'recovery'
  | 'succeeded'
  | 'failed'
  | 'cancelled'
  | 'unknown';

export type PublicTaskStepState = 'not_started' | 'started' | 'committed' | 'unknown';

export type PublicTaskRecoveryDecision =
  | 'resume'
  | 'rollback'
  | 'manual_recovery'
  | 'recovery'
  | 'unknown';

export type PublicTaskEvent = {
  reference: PublicTaskEventReference;
  taskReference: PublicTaskReference;
  occurredAtMs: number;
  kind: PublicTaskEventKind;
  displayState: PublicTaskEventDisplayState;
  status?: PublicTaskStatus;
  previousStatus?: PublicTaskStatus;
  nextStatus?: PublicTaskStatus;
  checkpointOrdinal?: number;
  checkpointState?: PublicTaskStepState;
  recoveryDecision?: PublicTaskRecoveryDecision;
};
