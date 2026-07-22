import type {
  McpCatalogDetail,
  McpCatalogListRequest,
  McpCatalogPage,
  McpEventsPage,
  McpHealthStatus,
  McpInstallConfirmRequest,
  McpManagedDetail,
  McpManagedPage,
  McpManagedSummary,
  McpManualConnectionInput,
  McpManualStdioSourcesPage,
  McpPlanIntent,
  McpPlanReview,
  McpPlatformErrorCode,
  McpPlatformErrorEnvelope,
  McpPlatformOutcome,
  McpSourcesPolicyState,
  McpTaskRef,
} from '@aaif/goose-sdk';
import { getAcpClient } from './acpConnection';

export type McpPlatformRecoveryViewModel = {
  title: string;
  message: string;
  nextStep: string;
  retryable: boolean;
  correlationId?: string;
  code?: McpPlatformErrorCode;
  kind?: 'service' | 'connection' | 'monitor';
};

const recoveryByCode: Partial<
  Record<McpPlatformErrorCode, Pick<McpPlatformRecoveryViewModel, 'title' | 'nextStep'>>
> = {
  credential_missing: {
    title: 'Credential reference required',
    nextStep: 'Choose an existing bearer credential reference, then create a new plan.',
  },
  remote_http_policy_unavailable: {
    title: 'Remote HTTP is unavailable by policy',
    nextStep: 'Review the machine policy or contact its administrator before retrying.',
  },
  manual_stdio_provider_unavailable: {
    title: 'No approved stdio provider is available',
    nextStep: 'Restore the approved provider source or contact the policy administrator.',
  },
  policy_denied: {
    title: 'Blocked by machine policy',
    nextStep: 'Review the policy reasons and contact the policy administrator if needed.',
  },
  plan_expired: {
    title: 'Plan expired',
    nextStep: 'Create and review a new plan before confirming.',
  },
  plan_stale: {
    title: 'Plan is no longer current',
    nextStep: 'Refresh the MCP state and create a new plan.',
  },
  revision_conflict: {
    title: 'MCP state changed',
    nextStep: 'Refresh the MCP details before trying this action again.',
  },
  repository_unavailable: {
    title: 'MCP data is temporarily unavailable',
    nextStep: 'Retry when the local MCP repository is available.',
  },
};

export class McpPlatformServiceError extends Error {
  constructor(
    public readonly envelope: McpPlatformErrorEnvelope,
    public readonly recovery: McpPlatformRecoveryViewModel
  ) {
    super(envelope.message);
    this.name = 'McpPlatformServiceError';
  }
}

export function mapMcpPlatformError(error: McpPlatformErrorEnvelope): McpPlatformRecoveryViewModel {
  const mapped = recoveryByCode[error.code];
  return {
    title: mapped?.title ?? 'MCP operation could not be completed',
    message: error.message,
    nextStep:
      mapped?.nextStep ??
      (error.retryable ? 'Retry the operation.' : 'Review the MCP state before continuing.'),
    retryable: error.retryable,
    correlationId: error.correlationId,
    code: error.code,
    kind: 'service',
  };
}

export function toMcpRecoveryViewModel(error: unknown): McpPlatformRecoveryViewModel {
  if (error instanceof McpPlatformServiceError) return error.recovery;
  return {
    title: 'MCP Center is unavailable',
    message: 'The MCP Platform request could not be completed.',
    nextStep: 'Check the Goose connection and retry.',
    retryable: true,
    kind: 'connection',
  };
}

export function unwrapMcpOutcome<T>(outcome: McpPlatformOutcome<T>): T {
  if (outcome.status === 'success') return outcome.value;
  throw new McpPlatformServiceError(outcome.error, mapMcpPlatformError(outcome.error));
}

export function createMcpIdempotencyKey(action: string): string {
  return `${action}-${window.crypto.randomUUID()}`;
}

export async function listMcpCatalog(params: McpCatalogListRequest): Promise<McpCatalogPage> {
  const client = await getAcpClient();
  return unwrapMcpOutcome((await client.mcpPlatform.mcpCatalogList_unstable(params)).outcome);
}

export async function getMcpCatalogDetail(manifestDigest: string): Promise<McpCatalogDetail> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (
      await client.mcpPlatform.mcpCatalogDetail_unstable({
        catalog: { type: 'manifest_digest', manifest_digest: manifestDigest },
      })
    ).outcome
  );
}

export async function createMcpPlan(intent: McpPlanIntent): Promise<McpPlanReview> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (
      await client.mcpPlatform.mcpPlanCreate_unstable({
        intent,
        idempotencyKey: createMcpIdempotencyKey('plan'),
      })
    ).outcome
  );
}

export async function createManualMcpPlan(
  connection: McpManualConnectionInput
): Promise<McpPlanReview> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (
      await client.mcpPlatform.mcpManualPlanCreate_unstable({
        connection,
        idempotencyKey: createMcpIdempotencyKey('manual-plan'),
      })
    ).outcome
  );
}

export async function confirmMcpPlan(
  plan: Pick<McpPlanReview, 'planId' | 'planDigest'>,
  decision: McpInstallConfirmRequest['userDecision']
): Promise<McpTaskRef> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (
      await client.mcpPlatform.mcpInstallConfirm_unstable({
        planId: plan.planId,
        planDigest: plan.planDigest,
        userDecision: decision,
        idempotencyKey: createMcpIdempotencyKey(`plan-${decision}`),
      })
    ).outcome
  );
}

export async function listManagedMcps(cursor?: string): Promise<McpManagedPage> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (await client.mcpPlatform.mcpList_unstable({ cursor, pageSize: 50 })).outcome
  );
}

export async function getManagedMcp(managedMcpId: string): Promise<McpManagedDetail> {
  const client = await getAcpClient();
  return unwrapMcpOutcome((await client.mcpPlatform.mcpGet_unstable({ managedMcpId })).outcome);
}

export async function runMcpHealth(
  managedMcpId: string,
  mode: 'registration' | 'runtime'
): Promise<McpTaskRef> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (
      await client.mcpPlatform.mcpHealthRun_unstable({
        managedMcpId,
        mode,
        idempotencyKey: createMcpIdempotencyKey('health'),
      })
    ).outcome
  );
}

export async function getMcpHealth(managedMcpId: string): Promise<McpHealthStatus> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (await client.mcpPlatform.mcpHealthGet_unstable({ managedMcpId })).outcome
  );
}

export async function setMcpDefaultEnabled(
  item: Pick<McpManagedSummary, 'managedMcpId' | 'revision'>,
  enabled: boolean
): Promise<McpManagedSummary> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (
      await client.mcpPlatform.mcpSetDefaultEnabled_unstable({
        managedMcpId: item.managedMcpId,
        enabled,
        expectedRevision: item.revision,
      })
    ).outcome
  );
}

export async function getMcpTask(taskId: string): Promise<McpTaskRef> {
  const client = await getAcpClient();
  return unwrapMcpOutcome((await client.mcpPlatform.mcpTaskGet_unstable({ taskId })).outcome);
}

export async function cancelMcpTask(
  task: Pick<McpTaskRef, 'taskId' | 'revision'>
): Promise<McpTaskRef> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (
      await client.mcpPlatform.mcpTaskCancel_unstable({
        taskId: task.taskId,
        expectedRevision: task.revision,
      })
    ).outcome
  );
}

export async function retryMcpTask(
  task: Pick<McpTaskRef, 'taskId' | 'revision'>
): Promise<McpTaskRef> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (
      await client.mcpPlatform.mcpTaskRetry_unstable({
        taskId: task.taskId,
        expectedRevision: task.revision,
        idempotencyKey: createMcpIdempotencyKey('task-retry'),
      })
    ).outcome
  );
}

export async function resumeMcpEvents(
  taskIds: string[],
  afterEventId?: number
): Promise<McpEventsPage> {
  const client = await getAcpClient();
  return unwrapMcpOutcome(
    (
      await client.mcpPlatform.mcpEventsResume_unstable({
        taskIds,
        afterEventId,
        limit: 100,
      })
    ).outcome
  );
}

export async function getMcpSourcesPolicy(): Promise<McpSourcesPolicyState> {
  const client = await getAcpClient();
  return unwrapMcpOutcome((await client.mcpPlatform.mcpSourcesPolicyGet_unstable()).outcome);
}

export async function listManualStdioSources(): Promise<McpManualStdioSourcesPage> {
  const client = await getAcpClient();
  return unwrapMcpOutcome((await client.mcpPlatform.mcpManualStdioSourcesList_unstable()).outcome);
}
