import { IntlProvider } from 'react-intl';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type {
  McpHealthStatus,
  McpManagedDetail,
  McpManagedSummary,
  McpPlanReview,
  McpTaskRef,
} from '@aaif/goose-sdk';
import { ManagedTab } from '../ManagedTab';
import {
  cancelMcpTask,
  confirmMcpPlan,
  createMcpPlan,
  getManagedMcp,
  getMcpHealth,
  listManagedMcps,
  retryMcpTask,
  runMcpHealth,
  setMcpDefaultEnabled,
} from '../../../acp/mcp-platform';

const monitor = vi.hoisted(() => ({
  onUpdate: null as ((task: McpTaskRef) => void) | null,
  onTerminal: null as ((task: McpTaskRef) => void) | null,
}));

vi.mock('../useMcpTaskMonitor', () => ({
  useMcpTaskMonitor: (
    _task: McpTaskRef | null,
    onUpdate: (task: McpTaskRef) => void,
    _onError: (cause: unknown) => void,
    onTerminal: (task: McpTaskRef) => void
  ) => {
    monitor.onUpdate = onUpdate;
    monitor.onTerminal = onTerminal;
  },
}));

vi.mock('../../../acp/mcp-platform', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../../acp/mcp-platform')>();
  return {
    ...actual,
    cancelMcpTask: vi.fn(),
    confirmMcpPlan: vi.fn(),
    createMcpPlan: vi.fn(),
    getManagedMcp: vi.fn(),
    getMcpHealth: vi.fn(),
    listManagedMcps: vi.fn(),
    retryMcpTask: vi.fn(),
    runMcpHealth: vi.fn(),
    setMcpDefaultEnabled: vi.fn(),
  };
});

const task = (overrides: Partial<McpTaskRef> = {}): McpTaskRef => ({
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
});

const summary = (
  managedMcpId: string,
  revision: number,
  overrides: Partial<McpManagedSummary> = {}
): McpManagedSummary => ({
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
});

const detail = (item: McpManagedSummary): McpManagedDetail => ({
  summary: item,
  distributionAdapter: item.distributionAdapter,
  activeManifestDigest: `manifest-${item.managedMcpId}`,
  activeVersion: item.activeVersion ?? '',
  extensionConfigKey: `extension-${item.managedMcpId}`,
  projectionDigest: `projection-${item.managedMcpId}`,
});

const health = (managedMcpId: string): McpHealthStatus => ({
  managedMcpId,
  state: 'healthy',
});

const plan = (managedMcpId: string): McpPlanReview => ({
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
});

function renderManaged(externalTask: McpTaskRef | null = null) {
  const onTaskChange = vi.fn();
  render(
    <IntlProvider locale="en">
      <ManagedTab externalTask={externalTask} onTaskChange={onTaskChange} />
    </IntlProvider>
  );
  return onTaskChange;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (cause: unknown) => void;
  const promise = new Promise<T>((next, fail) => {
    resolve = next;
    reject = fail;
  });
  return { promise, resolve, reject };
}

describe('ManagedTab freshness and task ownership', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    monitor.onUpdate = null;
    monitor.onTerminal = null;
  });

  it('uses the detail summary revision, version, and eligibility for the next mutation', async () => {
    const listed = summary('managed-a', 1);
    const fresh = summary('managed-a', 7, {
      availableVersion: '2.0.0',
      eligibility: {
        ...listed.eligibility,
        update: true,
        updateReason: 'eligible',
      },
    });
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [listed] });
    vi.mocked(getManagedMcp).mockResolvedValue(detail(fresh));
    vi.mocked(getMcpHealth).mockResolvedValue(health('managed-a'));
    vi.mocked(setMcpDefaultEnabled).mockResolvedValue({ ...fresh, defaultEnabled: true });
    const user = userEvent.setup();
    renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    expect(await screen.findByRole('button', { name: 'Review update' })).toBeInTheDocument();
    await user.click(screen.getByRole('switch', { name: 'Default enabled' }));

    expect(setMcpDefaultEnabled).toHaveBeenCalledWith(
      expect.objectContaining({ managedMcpId: 'managed-a', revision: 7 }),
      true
    );
  });

  it('does not apply a delayed default-enabled response for A to B', async () => {
    const itemA = summary('managed-a', 2);
    const itemB = summary('managed-b', 3);
    const pendingToggle = deferred<McpManagedSummary>();
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [itemA, itemB] });
    vi.mocked(getManagedMcp).mockImplementation(async (id) =>
      detail(id === 'managed-a' ? itemA : itemB)
    );
    vi.mocked(getMcpHealth).mockImplementation(async (id) => health(id));
    vi.mocked(setMcpDefaultEnabled).mockReturnValue(pendingToggle.promise);
    const user = userEvent.setup();
    renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await user.click(screen.getByRole('switch', { name: 'Default enabled' }));
    await user.click(screen.getByRole('button', { name: /MCP B/i }));
    await screen.findByRole('heading', { level: 2, name: 'MCP B' });
    await act(async () => pendingToggle.resolve({ ...itemA, defaultEnabled: true, revision: 4 }));

    expect(screen.getByRole('switch', { name: 'Default enabled' })).not.toBeChecked();
    expect(screen.getByRole('heading', { level: 2, name: 'MCP B' })).toBeInTheDocument();
  });

  it('prevents an older detail response from replacing a newer selection', async () => {
    const itemA = summary('managed-a', 1);
    const itemB = summary('managed-b', 1);
    const pendingA = deferred<McpManagedDetail>();
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [itemA, itemB] });
    vi.mocked(getManagedMcp).mockImplementation((id) =>
      id === 'managed-a' ? pendingA.promise : Promise.resolve(detail(itemB))
    );
    vi.mocked(getMcpHealth).mockImplementation(async (id) => health(id));
    const user = userEvent.setup();
    renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await user.click(screen.getByRole('button', { name: /MCP B/i }));
    expect(await screen.findByRole('heading', { level: 2, name: 'MCP B' })).toBeInTheDocument();
    await act(async () => pendingA.resolve(detail(itemA)));

    expect(screen.getByRole('heading', { level: 2, name: 'MCP B' })).toBeInTheDocument();
  });

  it('clears an unrelated task when switching to an MCP with no task', async () => {
    const itemA = summary('managed-a', 2, { currentTask: task() });
    const itemB = summary('managed-b', 3);
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [itemA, itemB] });
    vi.mocked(getManagedMcp).mockImplementation(async (id) =>
      detail(id === 'managed-a' ? itemA : itemB)
    );
    vi.mocked(getMcpHealth).mockImplementation(async (id) => health(id));
    const user = userEvent.setup();
    const onTaskChange = renderManaged(task());

    await user.click(await screen.findByRole('button', { name: /MCP B/i }));
    await screen.findByRole('heading', { level: 2, name: 'MCP B' });

    expect(screen.queryByText('Current task')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Cancel task' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Retry task' })).not.toBeInTheDocument();
    expect(onTaskChange).toHaveBeenCalledWith(null);
    expect(cancelMcpTask).not.toHaveBeenCalled();
    expect(retryMcpTask).not.toHaveBeenCalled();
  });

  it('keeps lifecycle actions hidden when the refreshed eligibility gates deny them', async () => {
    const item = summary('managed-a', 4, { availableVersion: '2.0.0' });
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [item] });
    vi.mocked(getManagedMcp).mockResolvedValue(detail(item));
    vi.mocked(getMcpHealth).mockResolvedValue(health('managed-a'));
    const user = userEvent.setup();
    renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await screen.findByRole('heading', { level: 2, name: 'MCP A' });

    expect(screen.queryByRole('button', { name: 'Review update' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Review repair' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Review uninstall' })).not.toBeInTheDocument();
  });

  it('refreshes the owning MCP after a terminal task revision', async () => {
    const running = task();
    const initial = summary('managed-a', 2, { currentTask: running });
    const terminal = task({ status: 'succeeded', progress: 100, revision: 3 });
    const refreshed = summary('managed-a', 9, { activeVersion: '2.0.0' });
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [initial] });
    vi.mocked(getManagedMcp)
      .mockResolvedValueOnce(detail(initial))
      .mockResolvedValueOnce(detail(refreshed));
    vi.mocked(getMcpHealth).mockResolvedValue(health('managed-a'));
    const user = userEvent.setup();
    renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await screen.findByText('Current task');
    await act(async () => {
      monitor.onUpdate?.(terminal);
      monitor.onTerminal?.(terminal);
    });

    await waitFor(() => expect(getManagedMcp).toHaveBeenCalledTimes(2));
    expect(await screen.findByText('2.0.0')).toBeInTheDocument();
  });

  it('does not let a delayed terminal refresh for A replace selection B', async () => {
    const running = task();
    const itemA = summary('managed-a', 2, { currentTask: running });
    const itemB = summary('managed-b', 3);
    const pendingRefresh = deferred<McpManagedDetail>();
    let aDetailCalls = 0;
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [itemA, itemB] });
    vi.mocked(getManagedMcp).mockImplementation((id) => {
      if (id === 'managed-b') return Promise.resolve(detail(itemB));
      aDetailCalls += 1;
      return aDetailCalls === 1 ? Promise.resolve(detail(itemA)) : pendingRefresh.promise;
    });
    vi.mocked(getMcpHealth).mockImplementation(async (id) => health(id));
    const user = userEvent.setup();
    renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await screen.findByText('Current task');
    act(() => monitor.onTerminal?.(task({ status: 'succeeded', revision: 3 })));
    await user.click(screen.getByRole('button', { name: /MCP B/i }));
    await screen.findByRole('heading', { level: 2, name: 'MCP B' });
    await act(async () =>
      pendingRefresh.resolve(detail(summary('managed-a', 9, { activeVersion: '2.0.0' })))
    );

    expect(screen.getByRole('heading', { level: 2, name: 'MCP B' })).toBeInTheDocument();
    expect(screen.queryByText('Current task')).not.toBeInTheDocument();
  });

  it('keeps a retried task when an older terminal refresh returns no current task', async () => {
    const failed = task({
      status: 'failed',
      progress: 100,
      revision: 3,
      cancellable: false,
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
    const initial = summary('managed-a', 2, { currentTask: task() });
    const pendingRefresh = deferred<McpManagedDetail>();
    const retried = task({ taskId: 'task-retry', status: 'queued', progress: 0, revision: 1 });
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [initial] });
    vi.mocked(getManagedMcp)
      .mockResolvedValueOnce(detail(initial))
      .mockReturnValueOnce(pendingRefresh.promise);
    vi.mocked(getMcpHealth).mockResolvedValue(health('managed-a'));
    vi.mocked(retryMcpTask).mockResolvedValue(retried);
    const user = userEvent.setup();
    const onTaskChange = renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await act(async () => {
      monitor.onUpdate?.(failed);
      monitor.onTerminal?.(failed);
    });
    await user.click(await screen.findByRole('button', { name: 'Retry task' }));
    await act(async () => pendingRefresh.resolve(detail(summary('managed-a', 3))));

    expect(await screen.findByText('Queued')).toBeInTheDocument();
    expect(onTaskChange).toHaveBeenLastCalledWith(retried);
  });

  it('keeps a confirmed task when its refresh returns an older empty detail', async () => {
    const initial = summary('managed-a', 2, {
      eligibility: { ...summary('managed-a', 2).eligibility, repair: true },
    });
    const pendingRefresh = deferred<McpManagedDetail>();
    const confirmed = task({ taskId: 'task-confirmed', operation: 'repair', revision: 1 });
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [initial] });
    vi.mocked(getManagedMcp)
      .mockResolvedValueOnce(detail(initial))
      .mockReturnValueOnce(pendingRefresh.promise);
    vi.mocked(getMcpHealth).mockResolvedValue(health('managed-a'));
    vi.mocked(createMcpPlan).mockResolvedValue(plan('managed-a'));
    vi.mocked(confirmMcpPlan).mockResolvedValue(confirmed);
    const user = userEvent.setup();
    const onTaskChange = renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await user.click(screen.getByRole('button', { name: 'Review repair' }));
    await user.click(screen.getByRole('button', { name: 'Confirm and start' }));
    await waitFor(() => expect(getManagedMcp).toHaveBeenCalledTimes(2));
    await act(async () => pendingRefresh.resolve(detail(summary('managed-a', 3))));

    expect(await screen.findByText('Running')).toBeInTheDocument();
    expect(onTaskChange).toHaveBeenLastCalledWith(confirmed);
  });

  it('does not let an old task detail overwrite a newer task generation', async () => {
    const failed = task({
      status: 'failed',
      progress: 100,
      revision: 3,
      cancellable: false,
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
    const initial = summary('managed-a', 2, { currentTask: task() });
    const pendingRefresh = deferred<McpManagedDetail>();
    const newer = task({ taskId: 'task-new', status: 'queued', progress: 0, revision: 1 });
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [initial] });
    vi.mocked(getManagedMcp)
      .mockResolvedValueOnce(detail(initial))
      .mockReturnValueOnce(pendingRefresh.promise);
    vi.mocked(getMcpHealth).mockResolvedValue(health('managed-a'));
    vi.mocked(retryMcpTask).mockResolvedValue(newer);
    const user = userEvent.setup();
    const onTaskChange = renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await act(async () => {
      monitor.onUpdate?.(failed);
      monitor.onTerminal?.(failed);
    });
    await user.click(await screen.findByRole('button', { name: 'Retry task' }));
    await act(async () =>
      pendingRefresh.resolve(
        detail(summary('managed-a', 3, { currentTask: task({ revision: 2 }) }))
      )
    );

    expect(await screen.findByText('Queued')).toBeInTheDocument();
    expect(onTaskChange).toHaveBeenLastCalledWith(newer);
  });

  it('does not let a delayed health action for A write task or health state into B', async () => {
    const itemA = summary('managed-a', 2);
    const itemB = summary('managed-b', 3);
    const pendingHealthTask = deferred<McpTaskRef>();
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [itemA, itemB] });
    vi.mocked(getManagedMcp).mockImplementation(async (id) =>
      detail(id === 'managed-a' ? itemA : itemB)
    );
    vi.mocked(getMcpHealth).mockImplementation(async (id) => health(id));
    vi.mocked(runMcpHealth).mockReturnValue(pendingHealthTask.promise);
    const user = userEvent.setup();
    const onTaskChange = renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await user.click(screen.getByRole('button', { name: 'Run health check' }));
    await user.click(screen.getByRole('button', { name: /MCP B/i }));
    await screen.findByRole('heading', { level: 2, name: 'MCP B' });
    await act(async () => pendingHealthTask.resolve(task({ operation: 'health' })));

    expect(screen.queryByText('Current task')).not.toBeInTheDocument();
    expect(onTaskChange).toHaveBeenLastCalledWith(null);
    expect(getMcpHealth).toHaveBeenCalledTimes(2);
  });

  it('opens a delayed plan with its original A ownership after selecting B', async () => {
    const itemA = summary('managed-a', 2, {
      eligibility: {
        ...summary('managed-a', 2).eligibility,
        repair: true,
      },
    });
    const itemB = summary('managed-b', 3);
    const pendingPlan = deferred<McpPlanReview>();
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [itemA, itemB] });
    vi.mocked(getManagedMcp).mockImplementation(async (id) =>
      detail(id === 'managed-a' ? itemA : itemB)
    );
    vi.mocked(getMcpHealth).mockImplementation(async (id) => health(id));
    vi.mocked(createMcpPlan).mockReturnValue(pendingPlan.promise);
    const user = userEvent.setup();
    renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await user.click(screen.getByRole('button', { name: 'Review repair' }));
    await user.click(screen.getByRole('button', { name: /MCP B/i }));
    await screen.findByRole('heading', { level: 2, name: 'MCP B' });
    await act(async () => pendingPlan.resolve(plan('managed-a')));

    expect(screen.getByRole('dialog', { name: 'Review MCP plan' })).toBeInTheDocument();
    expect(screen.getByText('managed-b')).toBeInTheDocument();
  });

  it('keeps the plan owner when confirmation finishes after selecting another MCP', async () => {
    const itemA = summary('managed-a', 2, {
      eligibility: {
        ...summary('managed-a', 2).eligibility,
        repair: true,
      },
    });
    const itemB = summary('managed-b', 3);
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [itemA, itemB] });
    vi.mocked(getManagedMcp).mockImplementation(async (id) =>
      detail(id === 'managed-a' ? itemA : itemB)
    );
    vi.mocked(getMcpHealth).mockImplementation(async (id) => health(id));
    vi.mocked(createMcpPlan).mockResolvedValue(plan('managed-a'));
    const pendingConfirmation = deferred<McpTaskRef>();
    vi.mocked(confirmMcpPlan).mockReturnValue(pendingConfirmation.promise);
    const user = userEvent.setup();
    renderManaged();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    const itemBButton = screen.getByRole('button', { name: /MCP B/i });
    await user.click(screen.getByRole('button', { name: 'Review repair' }));
    await user.click(screen.getByRole('button', { name: 'Confirm and start' }));
    fireEvent.click(itemBButton);
    await act(async () => pendingConfirmation.resolve(task({ operation: 'repair' })));

    await waitFor(() => expect(getManagedMcp).toHaveBeenLastCalledWith('managed-a'));
    expect(await screen.findByRole('heading', { level: 2, name: 'MCP B' })).toBeInTheDocument();
    expect(screen.queryByText('Current task')).not.toBeInTheDocument();
    expect(confirmMcpPlan).toHaveBeenCalledWith(
      expect.objectContaining({ planId: 'plan-managed-a' }),
      'confirm'
    );
  });

  it.each([
    ['cancel', cancelMcpTask, 'Cancel task'],
    ['retry', retryMcpTask, 'Retry task'],
  ] as const)(
    'does not let a delayed %s response for A replace B task state',
    async (_, action, label) => {
      const failedTask = task({
        outcome: {
          state: 'failed',
          error: {
            code: 'adapter_failed',
            retryable: true,
            correlationId: 'failure',
            message: 'failed',
            suggestion: 'retry',
          },
          rollback: 'not_required',
          finalization: 'complete',
          remainingEffects: [],
          nextAction: 'retry',
        },
      });
      const itemA = summary('managed-a', 2, { currentTask: failedTask });
      const itemB = summary('managed-b', 3);
      const pendingAction = deferred<McpTaskRef>();
      vi.mocked(listManagedMcps).mockResolvedValue({ items: [itemA, itemB] });
      vi.mocked(getManagedMcp).mockImplementation(async (id) =>
        detail(id === 'managed-a' ? itemA : itemB)
      );
      vi.mocked(getMcpHealth).mockImplementation(async (id) => health(id));
      vi.mocked(action).mockReturnValue(pendingAction.promise);
      const user = userEvent.setup();
      const onTaskChange = renderManaged();

      await user.click(await screen.findByRole('button', { name: /MCP A/i }));
      await user.click(await screen.findByRole('button', { name: label }));
      await user.click(screen.getByRole('button', { name: /MCP B/i }));
      await screen.findByRole('heading', { level: 2, name: 'MCP B' });
      await act(async () => pendingAction.resolve(task({ revision: 3 })));

      expect(screen.queryByText('Current task')).not.toBeInTheDocument();
      expect(onTaskChange).toHaveBeenLastCalledWith(null);
    }
  );

  it.each([
    ['cancel', cancelMcpTask, 'Cancel task'],
    ['retry', retryMcpTask, 'Retry task'],
  ] as const)(
    'hides a stale %s failure after the monitor advances the task',
    async (_, action, label) => {
      const actionable = task({
        status: label === 'Retry task' ? 'failed' : 'running',
        cancellable: label === 'Cancel task',
        outcome:
          label === 'Retry task'
            ? {
                state: 'failed',
                error: {
                  code: 'adapter_failed',
                  retryable: true,
                  correlationId: 'failure',
                  message: 'failed',
                  suggestion: 'retry',
                },
                rollback: 'not_required',
                finalization: 'complete',
                remainingEffects: [],
                nextAction: 'retry',
              }
            : task().outcome,
      });
      const initial = summary('managed-a', 2, { currentTask: actionable });
      const pendingAction = deferred<McpTaskRef>();
      vi.mocked(listManagedMcps).mockResolvedValue({ items: [initial] });
      vi.mocked(getManagedMcp).mockResolvedValue(detail(initial));
      vi.mocked(getMcpHealth).mockResolvedValue(health('managed-a'));
      vi.mocked(action).mockReturnValue(pendingAction.promise);
      const user = userEvent.setup();
      renderManaged();

      await user.click(await screen.findByRole('button', { name: /MCP A/i }));
      await user.click(await screen.findByRole('button', { name: label }));
      await act(async () => monitor.onUpdate?.(task({ ...actionable, revision: 3 })));
      await act(async () => pendingAction.reject(new Error('stale mutation failed')));

      expect(screen.queryByText('MCP Center is unavailable')).not.toBeInTheDocument();
      expect(screen.queryByText('The request could not be completed.')).not.toBeInTheDocument();
    }
  );
});
