import { IntlProvider } from 'react-intl';
import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  LuminaClient,
  type McpHealthStatus,
  type McpManagedDetail,
  type McpManagedSummary,
  type McpPlanReview,
  type McpTaskRef,
  type Stream,
} from '@hikerm/lumina-sdk';
import { ManagedTab } from '../ManagedTab';

const boundary = vi.hoisted(() => ({ client: null as LuminaClient | null }));

vi.mock('../../../acp/acpConnection', () => ({
  getAcpClient: async () => {
    if (!boundary.client) throw new Error('desktop boundary client is not configured');
    return boundary.client;
  },
}));

type RpcResult = Record<string, unknown>;
type RpcHandler = (params: Record<string, unknown>) => RpcResult | Promise<RpcResult>;

class DeferredAcpTransport {
  readonly calls: Array<{ method: string; params: Record<string, unknown> }> = [];
  readonly handlers = new Map<string, RpcHandler>();
  readonly stream: Stream;
  private closeReadable!: () => void;

  constructor() {
    let respond!: (id: string | number, result: RpcResult) => void;
    const readable: Stream['readable'] = new globalThis.ReadableStream({
      start: (controller) => {
        respond = (id, result) => controller.enqueue({ jsonrpc: '2.0', id, result });
        this.closeReadable = () => controller.close();
      },
    });
    const writable: Stream['writable'] = new globalThis.WritableStream({
      write: (message) => {
        if (!('method' in message) || !('id' in message) || message.id === null) return;
        const requestId = message.id;
        const params = Object.fromEntries(Object.entries(message.params ?? {}));
        this.calls.push({ method: message.method, params });
        const handler = this.handlers.get(message.method);
        if (!handler) throw new Error(`Unhandled ACP method: ${message.method}`);
        void Promise.resolve(handler(params)).then((result) => respond(requestId, result));
      },
    });
    this.stream = { readable, writable };
  }

  close() {
    this.closeReadable();
  }
}

function success(value: unknown): RpcResult {
  return { outcome: { status: 'success', value } };
}

function revisionConflict(correlationId: string): RpcResult {
  return {
    outcome: {
      status: 'error',
      error: {
        code: 'revision_conflict',
        message: 'the task revision changed',
        retryable: true,
        correlationId,
      },
    },
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

function task(overrides: Partial<McpTaskRef> = {}): McpTaskRef {
  return {
    taskId: 'task-a',
    operation: 'update',
    status: 'running',
    progress: 40,
    cancellable: true,
    revision: 2,
    updatedAtMs: 2,
    outcome: {
      state: 'pending',
      rollback: 'not_required',
      finalization: 'pending',
      remainingEffects: [],
      nextAction: 'wait',
    },
    ...overrides,
  };
}

function failedTask(): McpTaskRef {
  return task({
    status: 'failed',
    progress: 100,
    cancellable: false,
    revision: 3,
    outcome: {
      state: 'failed',
      error: {
        code: 'adapter_failed',
        retryable: true,
        correlationId: 'failed-task',
        message: 'failed',
        suggestion: 'retry',
      },
      rollback: 'not_required',
      finalization: 'complete',
      remainingEffects: [],
      nextAction: 'retry',
    },
  });
}

function summary(
  managedMcpId: string,
  revision: number,
  overrides: Partial<McpManagedSummary> = {}
): McpManagedSummary {
  return {
    managedMcpId,
    mcpId: managedMcpId === 'managed-a' ? 'MCP A' : 'MCP B',
    installationScope: 'user',
    registration: 'registered',
    installation: 'installed',
    runtime: 'stopped',
    health: 'healthy',
    defaultEnabled: false,
  revision,
  updatedAtMs: revision,
  distributionAdapter: 'archive',
  activeVersion: '1.0.0',
  availableVersion: null,
  availableManifestDigest: null,
  currentTask: null,
  recoveryRequired: false,
  eligibility: {
      update: false,
      repair: false,
      uninstall: false,
      reason: 'eligible',
      updateReason: 'no_update_available',
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
    ...overrides,
  };
}

function detail(item: McpManagedSummary): McpManagedDetail {
  return {
    summary: item,
    distributionAdapter: item.distributionAdapter,
    activeManifestDigest: `manifest-${item.managedMcpId}`,
    activeVersion: item.activeVersion ?? '',
    extensionConfigKey: `extension-${item.managedMcpId}`,
    projectionDigest: `projection-${item.managedMcpId}`,
    latestHealth: null,
    registrationTask: null,
  };
}

function health(managedMcpId: string): McpHealthStatus {
  return { managedMcpId, state: 'healthy', latest: null };
}

function plan(managedMcpId: string): McpPlanReview {
  return {
    planId: `plan-${managedMcpId}`,
    planDigest: `plan-digest-${managedMcpId}`,
    expiresAtMs: Date.now() + 60_000,
    sourceId: 'catalog',
    proof: { type: 'local_bytes' },
    trustTier: 'official',
    publisher: { id: 'publisher', name: 'Publisher', signingIdentities: [] },
    mcpId: managedMcpId,
    name: managedMcpId,
    version: '2.0.0',
    selectedManifestDigest: `manifest-${managedMcpId}`,
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
}

function renderManaged() {
  const onTaskChange = vi.fn();
  render(
    <IntlProvider locale="en">
      <ManagedTab externalTask={null} onTaskChange={onTaskChange} />
    </IntlProvider>
  );
  return onTaskChange;
}

describe('ManagedTab through the real Desktop adapter and SDK client', () => {
  let transport: DeferredAcpTransport;

  beforeEach(() => {
    transport = new DeferredAcpTransport();
    boundary.client = new LuminaClient(
      () => ({
        requestPermission: async () => {
          throw new Error('unexpected permission request');
        },
        sessionUpdate: async () => {},
      }),
      transport.stream
    );
  });

  afterEach(() => {
    boundary.client = null;
    transport.close();
  });

  it('keeps a deferred plan and its task owned by A when selection moves to B', async () => {
    const itemA = summary('managed-a', 2, {
      eligibility: { ...summary('managed-a', 2).eligibility, repair: true },
    });
    const itemB = summary('managed-b', 4);
    const planCreation = deferred<RpcResult>();
    const confirmedTask = task({ taskId: 'task-confirm-a' });
    let confirmed = false;
    transport.handlers.set('lumina.mcpList_unstable', () =>
      success({ items: [itemA, itemB], nextCursor: null })
    );
    transport.handlers.set('lumina.mcpGet_unstable', ({ managedMcpId }) => {
      if (managedMcpId === 'managed-b') return success(detail(itemB));
      return success(
        detail(confirmed ? { ...itemA, revision: 3, currentTask: confirmedTask } : itemA)
      );
    });
    transport.handlers.set('lumina.mcpHealthGet_unstable', ({ managedMcpId }) =>
      success(health(String(managedMcpId)))
    );
    transport.handlers.set('lumina.mcpPlanCreate_unstable', () => planCreation.promise);
    transport.handlers.set('lumina.mcpInstallConfirm_unstable', () => {
      confirmed = true;
      return success(confirmedTask);
    });
    transport.handlers.set('lumina.mcpTaskGet_unstable', () => success(confirmedTask));
    transport.handlers.set('lumina.mcpEventsResume_unstable', () =>
      success({ events: [], nextEventId: 0, tasks: [] })
    );
    const user = userEvent.setup();
    const onTaskChange = renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await user.click(screen.getByRole('button', { name: 'Review repair' }));
    await waitFor(() =>
      expect(transport.calls.some(({ method }) => method === 'lumina.mcpPlanCreate_unstable')).toBe(
        true
      )
    );
    await user.click(screen.getByRole('button', { name: /MCP B/i }));
    await screen.findByRole('heading', { level: 2, name: 'MCP B' });

    await act(async () => planCreation.resolve(success(plan('managed-a'))));
    await user.click(screen.getByRole('button', { name: 'Confirm and start' }));

    await waitFor(() => {
      expect(
        transport.calls.filter(({ method }) => method === 'lumina.mcpGet_unstable')
      ).toHaveLength(3);
    });
    expect(screen.getByRole('heading', { level: 2, name: 'MCP B' })).toBeInTheDocument();
    expect(screen.queryByText('Current task')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Cancel task' })).not.toBeInTheDocument();
    expect(onTaskChange).not.toHaveBeenCalledWith(confirmedTask);
    expect(transport.calls).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          method: 'lumina.mcpPlanCreate_unstable',
          params: expect.objectContaining({
            intent: { type: 'repair', managed_mcp_id: 'managed-a' },
          }),
        }),
        expect.objectContaining({
          method: 'lumina.mcpInstallConfirm_unstable',
          params: expect.objectContaining({
            planId: 'plan-managed-a',
            planDigest: 'plan-digest-managed-a',
            userDecision: 'confirm',
          }),
        }),
        { method: 'lumina.mcpGet_unstable', params: { managedMcpId: 'managed-a' } },
      ])
    );

    await user.click(screen.getByRole('button', { name: /MCP A/i }));
    expect(await screen.findByText('Current task')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Cancel task' })).toBeInTheDocument();
    expect(onTaskChange).toHaveBeenCalledWith(confirmedTask);
  });

  it('ignores delayed A health and detail responses after selection moves to B', async () => {
    const itemA = summary('managed-a', 2);
    const itemB = summary('managed-b', 4);
    const delayedDetail = deferred<RpcResult>();
    const delayedHealth = deferred<RpcResult>();
    transport.handlers.set('lumina.mcpList_unstable', () =>
      success({ items: [itemA, itemB], nextCursor: null })
    );
    transport.handlers.set('lumina.mcpGet_unstable', ({ managedMcpId }) =>
      managedMcpId === 'managed-a' ? delayedDetail.promise : success(detail(itemB))
    );
    transport.handlers.set('lumina.mcpHealthGet_unstable', ({ managedMcpId }) =>
      managedMcpId === 'managed-a' ? delayedHealth.promise : success(health('managed-b'))
    );
    const user = userEvent.setup();
    renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await user.click(screen.getByRole('button', { name: /MCP B/i }));
    await screen.findByRole('heading', { level: 2, name: 'MCP B' });
    await act(async () => {
      delayedDetail.resolve(success(detail(itemA)));
      delayedHealth.resolve(success(health('managed-a')));
    });

    expect(screen.getByRole('heading', { level: 2, name: 'MCP B' })).toBeInTheDocument();
    expect(
      transport.calls
        .filter(({ method }) => method === 'lumina.mcpGet_unstable')
        .map(({ params }) => params)
    ).toEqual([{ managedMcpId: 'managed-a' }, { managedMcpId: 'managed-b' }]);
  });

  it('ignores a stale cancel rejection after the monitor advances the same task', async () => {
    const running = task();
    const advanced = task({ revision: 3, updatedAtMs: 3, progress: 60 });
    const initial = summary('managed-a', 2, { currentTask: running });
    const monitorTask = deferred<RpcResult>();
    const cancelResponse = deferred<RpcResult>();
    let taskGetCalls = 0;
    transport.handlers.set('lumina.mcpList_unstable', () =>
      success({ items: [initial], nextCursor: null })
    );
    transport.handlers.set('lumina.mcpGet_unstable', () => success(detail(initial)));
    transport.handlers.set('lumina.mcpHealthGet_unstable', () => success(health('managed-a')));
    transport.handlers.set('lumina.mcpTaskGet_unstable', () => {
      taskGetCalls += 1;
      return taskGetCalls === 1 ? monitorTask.promise : success(advanced);
    });
    transport.handlers.set('lumina.mcpEventsResume_unstable', () =>
      success({ events: [], nextEventId: 0, tasks: [] })
    );
    transport.handlers.set('lumina.mcpTaskCancel_unstable', () => cancelResponse.promise);
    const user = userEvent.setup();
    const onTaskChange = renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await waitFor(() => expect(taskGetCalls).toBe(1));
    await user.click(screen.getByRole('button', { name: 'Cancel task' }));
    await act(async () => monitorTask.resolve(success(advanced)));
    expect(await screen.findByText(/60%/)).toBeInTheDocument();
    await act(async () => cancelResponse.resolve(revisionConflict('stale-cancel')));

    await waitFor(() => expect(screen.getByRole('button', { name: 'Cancel task' })).toBeEnabled());
    expect(screen.queryByText('MCP state changed')).not.toBeInTheDocument();
    expect(screen.queryByText('stale-cancel')).not.toBeInTheDocument();
    expect(onTaskChange).toHaveBeenLastCalledWith(advanced);
    expect(transport.calls).toEqual(
      expect.arrayContaining([
        {
          method: 'lumina.mcpTaskCancel_unstable',
          params: { taskId: 'task-a', expectedRevision: 2 },
        },
      ])
    );
  });

  it('ignores a stale retry rejection after terminal detail advances the same task', async () => {
    const running = task();
    const terminal = failedTask();
    const advanced = { ...terminal, revision: 4, updatedAtMs: 4 };
    const initial = summary('managed-a', 2, { currentTask: running });
    const terminalRefresh = deferred<RpcResult>();
    const retryResponse = deferred<RpcResult>();
    let detailCalls = 0;
    transport.handlers.set('lumina.mcpList_unstable', () =>
      success({ items: [initial], nextCursor: null })
    );
    transport.handlers.set('lumina.mcpGet_unstable', () => {
      detailCalls += 1;
      return detailCalls === 1 ? success(detail(initial)) : terminalRefresh.promise;
    });
    transport.handlers.set('lumina.mcpHealthGet_unstable', () => success(health('managed-a')));
    transport.handlers.set('lumina.mcpTaskGet_unstable', () => success(terminal));
    transport.handlers.set('lumina.mcpEventsResume_unstable', () =>
      success({ events: [], nextEventId: 0, tasks: [] })
    );
    transport.handlers.set('lumina.mcpTaskRetry_unstable', () => retryResponse.promise);
    const user = userEvent.setup();
    const onTaskChange = renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await screen.findByRole('button', { name: 'Retry task' });
    await waitFor(() => expect(detailCalls).toBe(2));
    await user.click(screen.getByRole('button', { name: 'Retry task' }));
    await act(async () =>
      terminalRefresh.resolve(success(detail({ ...initial, currentTask: advanced })))
    );
    await waitFor(() => expect(onTaskChange).toHaveBeenLastCalledWith(advanced));
    await act(async () => retryResponse.resolve(revisionConflict('stale-retry')));

    await waitFor(() => expect(screen.getByRole('button', { name: 'Retry task' })).toBeEnabled());
    expect(screen.queryByText('MCP state changed')).not.toBeInTheDocument();
    expect(screen.queryByText('stale-retry')).not.toBeInTheDocument();
    expect(transport.calls).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          method: 'lumina.mcpTaskRetry_unstable',
          params: expect.objectContaining({ taskId: 'task-a', expectedRevision: 3 }),
        }),
      ])
    );
  });

  it('preserves a retried task across a delayed terminal refresh and sends retry/cancel revisions', async () => {
    const running = task();
    const initial = summary('managed-a', 2, { currentTask: running });
    const terminal = failedTask();
    const retried = task({ taskId: 'task-retried', status: 'queued', progress: 0, revision: 1 });
    const cancelling = task({
      taskId: 'task-retried',
      status: 'cancelling',
      progress: 0,
      revision: 2,
    });
    const terminalRefresh = deferred<RpcResult>();
    let detailCalls = 0;
    transport.handlers.set('lumina.mcpList_unstable', () =>
      success({ items: [initial], nextCursor: null })
    );
    transport.handlers.set('lumina.mcpGet_unstable', () => {
      detailCalls += 1;
      return detailCalls === 1 ? success(detail(initial)) : terminalRefresh.promise;
    });
    transport.handlers.set('lumina.mcpHealthGet_unstable', () => success(health('managed-a')));
    transport.handlers.set('lumina.mcpTaskGet_unstable', ({ taskId }) =>
      success(taskId === 'task-a' ? terminal : retried)
    );
    transport.handlers.set('lumina.mcpEventsResume_unstable', () =>
      success({ events: [], nextEventId: 0, tasks: [] })
    );
    transport.handlers.set('lumina.mcpTaskRetry_unstable', () => success(retried));
    transport.handlers.set('lumina.mcpTaskCancel_unstable', () => success(cancelling));
    const user = userEvent.setup();
    const onTaskChange = renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await user.click(await screen.findByRole('button', { name: 'Retry task' }));
    await act(async () => terminalRefresh.resolve(success(detail(summary('managed-a', 3)))));
    expect((await screen.findAllByText('Queued')).length).toBeGreaterThan(0);
    await user.click(screen.getByRole('button', { name: 'Cancel task' }));

    expect((await screen.findAllByText('Cancelling')).length).toBeGreaterThan(0);
    expect(onTaskChange).toHaveBeenLastCalledWith(cancelling);
    expect(transport.calls).toEqual(
      expect.arrayContaining([
        {
          method: 'lumina.mcpTaskRetry_unstable',
          params: expect.objectContaining({ taskId: 'task-a', expectedRevision: 3 }),
        },
        {
          method: 'lumina.mcpTaskCancel_unstable',
          params: { taskId: 'task-retried', expectedRevision: 1 },
        },
      ])
    );
  });
});
