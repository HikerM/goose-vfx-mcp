import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { McpManagedSummary, McpPlanReview, McpTaskRef } from '@aaif/goose-sdk';
import { McpPlatformClient } from '@aaif/goose-sdk';
import {
  cancelMcpTask,
  confirmMcpPlan,
  createMcpPlan,
  retryMcpTask,
  setMcpDefaultEnabled,
} from '../mcp-platform';

const boundary = vi.hoisted(() => ({
  client: null as { mcpPlatform: McpPlatformClient } | null,
}));

vi.mock('../acpConnection', () => ({
  getAcpClient: async () => {
    if (!boundary.client) throw new Error('boundary client is not configured');
    return boundary.client;
  },
}));

const task: McpTaskRef = {
  taskId: 'task-managed-a',
  operation: 'update',
  status: 'queued',
  progress: 0,
  cancellable: true,
  revision: 8,
  updatedAtMs: 8,
  outcome: {
    state: 'pending',
    rollback: 'not_required',
    finalization: 'pending',
    remainingEffects: [],
    nextAction: 'wait',
  },
};

const plan: McpPlanReview = {
  planId: 'plan-managed-a',
  planDigest: 'digest-managed-a',
  expiresAtMs: Date.now() + 60_000,
  sourceId: 'catalog',
  proof: { type: 'local_bytes' },
  trustTier: 'official',
  publisher: { id: 'publisher', name: 'Publisher', signingIdentities: [] },
  mcpId: 'managed-a',
  name: 'Managed A',
  version: '2.0.0',
  selectedManifestDigest: 'manifest-managed-a',
  immutableEvidence: { type: 'artifact', sha256: 'sha256:test' },
  permissions: [],
  networkOrigins: [],
  fileEffects: { writesFiles: false, removesFiles: false, ownedItems: 0 },
  hostEffects: { registrationIds: [] },
  processEffects: { processRequiredForConnection: false, startsDuringConfirmation: false },
  reversibility: { reversible: true, strategy: { type: 'remove_connection_registration' } },
  policy: { outcome: 'allow', reasons: [] },
  warnings: [],
  requiredConfirmations: [],
  defaultDisabled: true,
  recovery: 'none',
};

const summary: McpManagedSummary = {
  managedMcpId: 'managed-a',
  mcpId: 'managed-a',
  installationScope: 'user',
  registration: 'registered',
  installation: 'installed',
  runtime: 'stopped',
  health: 'healthy',
  defaultEnabled: true,
  revision: 18,
  updatedAtMs: 18,
  distributionAdapter: 'archive',
  recoveryRequired: false,
  eligibility: {
    update: true,
    repair: true,
    uninstall: true,
    reason: 'eligible',
    updateReason: 'eligible',
    repairReason: 'eligible',
    uninstallReason: 'eligible',
  },
  nextAction: 'none',
  phaseCapabilities: {
    sessionEnablement: 'not_available_in_this_phase',
    toolPolicy: 'not_available_in_this_phase',
    profiles: 'not_available_in_this_phase',
    modelSuggestions: 'not_available_in_this_phase',
  },
};

describe('Desktop adapter to formal ACP MCP Platform boundary', () => {
  const calls: Array<{ method: string; params: Record<string, unknown> }> = [];

  beforeEach(() => {
    calls.length = 0;
    boundary.client = {
      mcpPlatform: new McpPlatformClient({
        async extMethod(method, params) {
          calls.push({ method, params });
          const value =
            method === 'goose.mcpPlanCreate_unstable'
              ? plan
              : method === 'goose.mcpSetDefaultEnabled_unstable'
                ? summary
                : task;
          return { outcome: { status: 'success', value } };
        },
      }),
    };
  });

  it('carries plan owner, confirmation identity, and expected revisions through both real layers', async () => {
    const review = await createMcpPlan({
      type: 'update',
      managed_mcp_id: 'managed-a',
      target_version: '2.0.0',
    });
    await confirmMcpPlan(review, 'confirm');
    await setMcpDefaultEnabled({ managedMcpId: 'managed-a', revision: 17 }, true);
    await cancelMcpTask({ taskId: 'task-managed-a', revision: 8 });
    await retryMcpTask({ taskId: 'task-managed-a', revision: 9 });

    expect(calls.map(({ method }) => method)).toEqual([
      'goose.mcpPlanCreate_unstable',
      'goose.mcpInstallConfirm_unstable',
      'goose.mcpSetDefaultEnabled_unstable',
      'goose.mcpTaskCancel_unstable',
      'goose.mcpTaskRetry_unstable',
    ]);
    expect(calls[0].params).toMatchObject({
      intent: { type: 'update', managed_mcp_id: 'managed-a', target_version: '2.0.0' },
    });
    expect(calls[1].params).toMatchObject({
      planId: 'plan-managed-a',
      planDigest: 'digest-managed-a',
      userDecision: 'confirm',
    });
    expect(calls[2].params).toEqual({
      managedMcpId: 'managed-a',
      enabled: true,
      expectedRevision: 17,
    });
    expect(calls[3].params).toEqual({ taskId: 'task-managed-a', expectedRevision: 8 });
    expect(calls[4].params).toMatchObject({
      taskId: 'task-managed-a',
      expectedRevision: 9,
    });
  });

  it('preserves a formal deny envelope until the desktop recovery boundary', async () => {
    boundary.client = {
      mcpPlatform: new McpPlatformClient({
        async extMethod(method, params) {
          calls.push({ method, params });
          return {
            outcome: {
              status: 'error',
              error: {
                code: 'policy_denied',
                message: 'denied',
                retryable: false,
                correlationId: 'deny-correlation',
                details: { type: 'policy_decision', reason_codes: ['machine_policy'] },
              },
            },
          };
        },
      }),
    };

    await expect(
      createMcpPlan({ type: 'repair', managed_mcp_id: 'managed-a' })
    ).rejects.toMatchObject({
      envelope: {
        code: 'policy_denied',
        details: { type: 'policy_decision', reason_codes: ['machine_policy'] },
      },
      recovery: { code: 'policy_denied', retryable: false },
    });
    expect(calls[0].method).toBe('goose.mcpPlanCreate_unstable');
  });
});
