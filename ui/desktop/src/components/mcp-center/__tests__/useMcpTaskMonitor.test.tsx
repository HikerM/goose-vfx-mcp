import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { McpEventsPage, McpTaskRef } from '@aaif/goose-sdk';
import { useMcpTaskMonitor } from '../useMcpTaskMonitor';
import { getMcpTask, resumeMcpEvents } from '../../../acp/mcp-platform';

vi.mock('../../../acp/mcp-platform', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../../acp/mcp-platform')>();
  return { ...actual, getMcpTask: vi.fn(), resumeMcpEvents: vi.fn() };
});

const task = (revision: number, status: McpTaskRef['status'] = 'running'): McpTaskRef => ({
  taskId: 'task-1',
  operation: 'update',
  status,
  progress: revision,
  cancellable: true,
  revision,
  updatedAtMs: revision,
  outcome: {
    state: status === 'succeeded' ? 'succeeded' : 'pending',
    rollback: 'not_required',
    finalization: status === 'succeeded' ? 'complete' : 'pending',
    remainingEffects: [],
    nextAction: status === 'succeeded' ? 'none' : 'wait',
  },
});

const events = (tasks: McpTaskRef[] = []): McpEventsPage => ({
  events: [],
  nextEventId: 1,
  tasks,
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

describe('useMcpTaskMonitor', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it('keeps one refresh in flight and schedules the next refresh sequentially', async () => {
    const pending = deferred<McpTaskRef>();
    vi.mocked(getMcpTask).mockReturnValueOnce(pending.promise).mockResolvedValue(task(3));
    vi.mocked(resumeMcpEvents).mockResolvedValue(events());
    renderHook(() => useMcpTaskMonitor(task(1), vi.fn(), vi.fn(), vi.fn(), 0));

    await act(async () => vi.advanceTimersByTime(10_000));
    expect(getMcpTask).toHaveBeenCalledTimes(1);

    await act(async () => pending.resolve(task(2)));
    await act(async () => vi.advanceTimersByTime(1_999));
    expect(getMcpTask).toHaveBeenCalledTimes(1);
    await act(async () => vi.advanceTimersByTime(1));
    expect(getMcpTask).toHaveBeenCalledTimes(2);
  });

  it('ignores stale revisions and reports monitoring failures', async () => {
    const onUpdate = vi.fn();
    const onError = vi.fn();
    vi.mocked(getMcpTask)
      .mockResolvedValueOnce(task(4))
      .mockRejectedValueOnce(new Error('private endpoint detail'));
    vi.mocked(resumeMcpEvents).mockResolvedValue(events([task(3)]));
    renderHook(() => useMcpTaskMonitor(task(5), onUpdate, onError, vi.fn(), 0));

    await act(async () => Promise.resolve());
    expect(onUpdate).not.toHaveBeenCalled();
    await act(async () => vi.advanceTimersByTime(2_000));
    expect(onError).toHaveBeenCalledTimes(1);
  });
});
