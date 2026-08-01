import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { McpEventsPage, McpTaskRef } from '@aaif/goose-sdk';
import { useMcpTaskMonitor } from '../useMcpTaskMonitor';
import { getMcpTask, resumeMcpEvents } from '../../../acp/mcp-platform';

vi.mock('../../../acp/mcp-platform', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../../acp/mcp-platform')>();
  return { ...actual, getMcpTask: vi.fn(), resumeMcpEvents: vi.fn() };
});

const task = (
  revision: number,
  status: McpTaskRef['status'] = 'running',
  taskId = 'task-1'
): McpTaskRef => ({
  taskId,
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

const events = (items: McpEventsPage['events'] = [], tasks: McpTaskRef[] = []): McpEventsPage => ({
  events: items,
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

async function flushMonitor() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe('useMcpTaskMonitor', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it('keeps one refresh in flight and schedules the next refresh sequentially', async () => {
    const pending = deferred<McpTaskRef>();
    vi.mocked(getMcpTask).mockReturnValueOnce(pending.promise).mockResolvedValue(task(3));
    vi.mocked(resumeMcpEvents).mockResolvedValue(events());
    const onUpdate = vi.fn();
    const onError = vi.fn();
    const onTerminal = vi.fn();

    renderHook(() => useMcpTaskMonitor(task(1), onUpdate, onError, onTerminal, 0));

    await act(async () => vi.advanceTimersByTime(10_000));
    expect(getMcpTask).toHaveBeenCalledTimes(1);

    await act(async () => {
      pending.resolve(task(2));
      await Promise.resolve();
      await Promise.resolve();
    });
    await act(async () => vi.advanceTimersByTime(1_999));
    expect(getMcpTask).toHaveBeenCalledTimes(1);
    await act(async () => vi.advanceTimersByTime(1));
    expect(getMcpTask).toHaveBeenCalledTimes(2);
    expect(onError).not.toHaveBeenCalled();
  });

  it('ignores stale revisions, reports the first monitoring failure, and pauses polling', async () => {
    const onUpdate = vi.fn();
    const onError = vi.fn();
    const onTerminal = vi.fn();
    vi.mocked(getMcpTask)
      .mockResolvedValueOnce(task(4))
      .mockRejectedValueOnce(new Error('private endpoint detail'));
    vi.mocked(resumeMcpEvents).mockResolvedValue(events([], [task(3)]));
    const { result } = renderHook(() =>
      useMcpTaskMonitor(task(5), onUpdate, onError, onTerminal, 0)
    );

    await flushMonitor();
    expect(onUpdate).not.toHaveBeenCalled();
    await act(async () => vi.advanceTimersByTime(2_000));
    expect(onError).toHaveBeenCalledTimes(1);
    expect(result.current.paused).toBe(true);
    await act(async () => vi.advanceTimersByTime(20_000));
    expect(getMcpTask).toHaveBeenCalledTimes(2);
    expect(resumeMcpEvents).toHaveBeenCalledTimes(2);
    expect(onError).toHaveBeenCalledTimes(1);
    expect(onTerminal).not.toHaveBeenCalled();
  });

  it('restarts monitoring only when retryKey changes after a paused error', async () => {
    const onUpdate = vi.fn();
    const onError = vi.fn();
    const onTerminal = vi.fn();
    vi.mocked(getMcpTask)
      .mockRejectedValueOnce(new Error('Authorization: Bearer secret'))
      .mockResolvedValueOnce(task(2));
    vi.mocked(resumeMcpEvents).mockResolvedValue(events([], [task(2)]));

    const { result, rerender } = renderHook(
      ({ retryKey }) => useMcpTaskMonitor(task(1), onUpdate, onError, onTerminal, retryKey),
      { initialProps: { retryKey: 0 } }
    );

    await flushMonitor();
    expect(onError).toHaveBeenCalledTimes(1);
    expect(result.current.paused).toBe(true);
    expect(getMcpTask).toHaveBeenCalledTimes(1);

    await act(async () => vi.advanceTimersByTime(10_000));
    expect(getMcpTask).toHaveBeenCalledTimes(1);

    rerender({ retryKey: 1 });
    await flushMonitor();

    expect(result.current.paused).toBe(false);
    expect(getMcpTask).toHaveBeenCalledTimes(2);
    expect(onUpdate).toHaveBeenCalledWith(task(2));
    expect(onError).toHaveBeenCalledTimes(1);
  });

  it('restarts monitoring when the task identity changes after a paused error', async () => {
    const onUpdate = vi.fn();
    const onError = vi.fn();
    const onTerminal = vi.fn();
    vi.mocked(getMcpTask)
      .mockRejectedValueOnce(new Error('env={"TOKEN":"abc"}'))
      .mockResolvedValueOnce(task(2, 'running', 'task-2'));
    vi.mocked(resumeMcpEvents).mockResolvedValue(events([], [task(2, 'running', 'task-2')]));

    const { result, rerender } = renderHook(
      ({ currentTask }) => useMcpTaskMonitor(currentTask, onUpdate, onError, onTerminal, 0),
      { initialProps: { currentTask: task(1, 'running', 'task-1') } }
    );

    await flushMonitor();
    expect(result.current.paused).toBe(true);
    expect(onError).toHaveBeenCalledTimes(1);

    rerender({ currentTask: task(1, 'running', 'task-2') });
    await flushMonitor();

    expect(result.current.paused).toBe(false);
    expect(getMcpTask).toHaveBeenCalledTimes(2);
    expect(resumeMcpEvents).toHaveBeenCalledWith(['task-2'], undefined);
    expect(onUpdate).toHaveBeenCalledWith(task(2, 'running', 'task-2'));
  });

  it('deduplicates and orders resumed events before exposing them to the UI', async () => {
    vi.mocked(getMcpTask).mockResolvedValue(task(3));
    vi.mocked(resumeMcpEvents)
      .mockResolvedValueOnce(
        events([
          {
            eventId: 8,
            taskId: 'task-1',
            taskLocalSequence: 3,
            occurredAtMs: 8,
            actor: 'system',
            eventType: 'task_status_changed',
            payload: { type: 'task_status_changed', from: 'queued', to: 'running' },
          },
          {
            eventId: 7,
            taskId: 'task-1',
            taskLocalSequence: 2,
            occurredAtMs: 7,
            actor: 'system',
            eventType: 'task_created',
            payload: { type: 'task_created', status: 'queued' },
          },
        ])
      )
      .mockResolvedValueOnce(
        events([
          {
            eventId: 8,
            taskId: 'task-1',
            taskLocalSequence: 3,
            occurredAtMs: 8,
            actor: 'system',
            eventType: 'task_status_changed',
            payload: { type: 'task_status_changed', from: 'queued', to: 'running' },
          },
        ])
      );

    const onUpdate = vi.fn();
    const onError = vi.fn();
    const onTerminal = vi.fn();
    const { result } = renderHook(() =>
      useMcpTaskMonitor(task(1), onUpdate, onError, onTerminal, 0)
    );

    await flushMonitor();
    expect(result.current.events.map((event) => event.eventId)).toEqual([7, 8]);

    await act(async () => vi.advanceTimersByTime(2_000));
    expect(result.current.events.map((event) => event.eventId)).toEqual([7, 8]);
    expect(result.current.paused).toBe(false);
  });

  it('loads one safe event snapshot for a terminal task without scheduling another poll', async () => {
    vi.mocked(getMcpTask).mockResolvedValue(task(4, 'succeeded'));
    vi.mocked(resumeMcpEvents).mockResolvedValue(
      events([
        {
          eventId: 9,
          taskId: 'task-1',
          taskLocalSequence: 4,
          occurredAtMs: 9,
          actor: 'system',
          eventType: 'task_status_changed',
          payload: { type: 'task_status_changed', from: 'running', to: 'succeeded' },
        },
      ])
    );

    const onUpdate = vi.fn();
    const onError = vi.fn();
    const onTerminal = vi.fn();
    const { result } = renderHook(() =>
      useMcpTaskMonitor(task(4, 'succeeded'), onUpdate, onError, onTerminal, 0)
    );

    await flushMonitor();
    expect(result.current.eventsLoaded).toBe(true);
    expect(result.current.events).toHaveLength(1);
    expect(result.current.paused).toBe(false);

    await act(async () => vi.advanceTimersByTime(10_000));
    expect(getMcpTask).toHaveBeenCalledTimes(1);
    expect(resumeMcpEvents).toHaveBeenCalledTimes(1);
  });

  it('stops applying async results after unmount cleanup', async () => {
    const pendingTask = deferred<McpTaskRef>();
    const pendingEvents = deferred<McpEventsPage>();
    const onUpdate = vi.fn();
    const onError = vi.fn();
    const onTerminal = vi.fn();
    vi.mocked(getMcpTask).mockReturnValueOnce(pendingTask.promise);
    vi.mocked(resumeMcpEvents).mockReturnValueOnce(pendingEvents.promise);

    const { unmount } = renderHook(() =>
      useMcpTaskMonitor(task(1), onUpdate, onError, onTerminal, 0)
    );

    unmount();

    await act(async () => {
      pendingTask.resolve(task(2));
      pendingEvents.resolve(events([], [task(2)]));
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(onUpdate).not.toHaveBeenCalled();
    expect(onError).not.toHaveBeenCalled();
    expect(onTerminal).not.toHaveBeenCalled();
  });
});
