import { afterEach, beforeEach, describe, expect, expectTypeOf, it } from 'vitest';
import type { McpEventsPage, McpTaskRef } from '@hikerm/lumina-sdk';
import type { McpCenterSafeError, PublicTask, PublicTaskEvent } from '../../public-types';
import {
  __resetOpaqueReferenceStoreForTests,
  __setOpaqueReferenceStoreLimitsForTests,
  __setOpaqueReferenceStoreNowForTests,
  createMcpTaskMonitorCursor,
  getMcpTaskMonitorResumeRequest,
  mergeMcpTaskMonitorPage,
  normalizePublicTaskNextAction,
  normalizePublicTaskOperation,
  normalizePublicTaskRemainingEffect,
  normalizePublicTaskStatus,
  releaseMcpTaskMonitorCursor,
  resolvePublicTaskEventReference,
  resolvePublicTaskReference,
  revokePublicTaskReference,
  toMcpCenterSafeError,
  toPublicTask,
  toPublicTaskEvent,
} from '../mcpTaskPublicAdapter';

function task(overrides: Partial<McpTaskRef> = {}): McpTaskRef {
  return {
    taskId: 'task-1',
    operation: 'update',
    status: 'running',
    progress: 42,
    cancellable: true,
    revision: 1,
    updatedAtMs: 100,
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

function events(items: McpEventsPage['events'], tasks: McpTaskRef[] = []): McpEventsPage {
  return {
    events: items,
    nextEventId: 9,
    tasks,
  };
}

function mutateOpaqueId(opaqueId: string): string {
  const lastChar = opaqueId.slice(-1);
  if (!lastChar) return `${opaqueId}x`;
  const replacement = lastChar.toLowerCase() === 'a' ? 'b' : 'a';
  return `${opaqueId.slice(0, -1)}${replacement}`;
}

function legacyHashedSupportRef(code: string, source: string): string {
  let hash = 0x811c9dc5;
  for (let index = 0; index < `${code}:${source}`.length; index += 1) {
    hash ^= `${code}:${source}`.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return `MCP-${(hash >>> 0).toString(16).padStart(8, '0').toUpperCase()}`;
}

describe('mcpTaskPublicAdapter', () => {
  let now = 1_000;

  beforeEach(() => {
    __resetOpaqueReferenceStoreForTests();
    __setOpaqueReferenceStoreNowForTests(() => now);
  });

  afterEach(() => {
    __resetOpaqueReferenceStoreForTests();
    now = 1_000;
  });

  it('maps raw tasks and errors into a UI-safe public contract', () => {
    const publicTask = toPublicTask(
      task({
        status: 'failed',
        outcome: {
          state: 'failed',
          rollback: 'incomplete',
          finalization: 'recovery_required',
          remainingEffects: ['managed_installation'],
          nextAction: 'retry',
          error: {
            code: 'adapter_failed',
            retryable: true,
            correlationId: 'corr-private-123',
            message: 'Authorization: Bearer secret',
            suggestion: 'retry',
          },
        },
      })
    );

    expect(publicTask.reference.kind).toBe('mcp_task');
    expect(publicTask.status).toBe('failed');
    expect(publicTask.displayState).toBe('failed');
    expect(publicTask.outcome.error).toEqual({
      code: 'adapter_failed',
      retryable: true,
      supportRef: expect.stringMatching(/^MCP-[0-9A-F]{8}$/),
      userCopyKey: 'mcpCenter.safeError.adapter_failed',
    });
    expect(publicTask.capabilities.retry).toBe('available');
    expect('taskId' in publicTask).toBe(false);
    expect('correlationId' in (publicTask.outcome.error ?? {})).toBe(false);
    expect('message' in (publicTask.outcome.error ?? {})).toBe(false);
  });

  it('maps unknown raw values into safe fallback buckets', () => {
    expect(normalizePublicTaskStatus('sideways')).toBe('unknown');
    expect(normalizePublicTaskOperation('migrate')).toBe('unknown');
    expect(normalizePublicTaskRemainingEffect('digest_leak')).toBe('unknown');
    expect(normalizePublicTaskNextAction('resolve_recovery')).toBe('recovery');
  });

  it('fails closed for unknown or missing checkpoint states in step_status_changed events', () => {
    const baseEvent = {
      eventId: 7,
      taskId: 'task-1',
      taskLocalSequence: 2,
      occurredAtMs: 700,
      actor: 'system',
      eventType: 'step_status_changed',
    } as const;

    expect(
      toPublicTaskEvent({
        ...baseEvent,
        payload: { type: 'step_status_changed', ordinal: 1, from: 'not_started', to: 'started' },
      })
    ).toMatchObject({
      kind: 'step_status_changed',
      checkpointState: 'started',
      displayState: 'active',
    });

    expect(
      toPublicTaskEvent({
        ...baseEvent,
        eventId: 8,
        payload: { type: 'step_status_changed', ordinal: 2, from: 'started', to: 'committed' },
      })
    ).toMatchObject({
      kind: 'step_status_changed',
      checkpointState: 'committed',
      displayState: 'succeeded',
    });

    expect(
      toPublicTaskEvent({
        ...baseEvent,
        eventId: 9,
        payload: { type: 'step_status_changed', ordinal: 3, from: 'started', to: 'sideways' },
      })
    ).toMatchObject({
      kind: 'step_status_changed',
      checkpointState: 'unknown',
      displayState: 'unknown',
    });

    expect(
      toPublicTaskEvent({
        ...baseEvent,
        eventId: 10,
        payload: { type: 'step_status_changed', ordinal: 4 },
      })
    ).toMatchObject({
      kind: 'step_status_changed',
      checkpointState: 'unknown',
      displayState: 'unknown',
    });
  });

  it('merges out-of-order events by raw event id and only promotes newer task revisions', () => {
    const initialRawTask = task({ revision: 1, status: 'running' });
    const initialPublicTask = toPublicTask(initialRawTask);
    const initialCursor = createMcpTaskMonitorCursor(initialRawTask);

    const firstMerge = mergeMcpTaskMonitorPage(
      initialCursor,
      initialPublicTask,
      [],
      task({ revision: 2, status: 'running' }),
      events(
        [
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
        ],
        [task({ revision: 3, status: 'succeeded', progress: 100 })]
      )
    );

    expect(firstMerge.task.revision).toBe(3);
    expect(firstMerge.task.status).toBe('succeeded');
    expect(firstMerge.events.map((event) => event.kind)).toEqual(['created', 'status_changed']);
    expect(firstMerge.events.every((event) => !('eventId' in event))).toBe(true);

    const secondMerge = mergeMcpTaskMonitorPage(
      firstMerge.cursor,
      firstMerge.task,
      firstMerge.events,
      task({ revision: 3, status: 'succeeded', progress: 100 }),
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

    expect(secondMerge.taskChanged).toBe(false);
    expect(secondMerge.events).toHaveLength(2);
    expect(secondMerge.terminal).toBe(true);
  });

  it('revokes task and event references without exposing raw ids', () => {
    const publicTask = toPublicTask(task({ taskId: 'task-private-1' }));
    const publicEvent = toPublicTaskEvent(
      {
        eventId: 11,
        taskId: 'task-private-1',
        taskLocalSequence: 3,
        occurredAtMs: 800,
        actor: 'system',
        eventType: 'task_status_changed',
        payload: { type: 'task_status_changed', from: 'queued', to: 'running' },
      },
      publicTask.reference
    );

    expect(JSON.stringify(publicTask.reference)).not.toContain('task-private-1');
    expect(JSON.stringify(publicEvent.reference)).not.toContain('task-private-1');
    expect(resolvePublicTaskReference(publicTask.reference)).toEqual({ taskId: 'task-private-1' });
    expect(resolvePublicTaskEventReference(publicEvent.reference)).toEqual({
      eventId: 11,
      taskId: 'task-private-1',
    });

    revokePublicTaskReference(publicTask.reference);

    expect(resolvePublicTaskReference(publicTask.reference)).toBeNull();
    expect(resolvePublicTaskEventReference(publicEvent.reference)).toBeNull();
  });

  it('isolates same-task public refs so revoking one owner does not tear down siblings', () => {
    const sharedRawTask = task({ taskId: 'task-shared-owners' });
    const publicTaskA = toPublicTask(sharedRawTask);
    const publicTaskB = toPublicTask(sharedRawTask);
    const rawEvent = {
      eventId: 15,
      taskId: 'task-shared-owners',
      taskLocalSequence: 1,
      occurredAtMs: 915,
      actor: 'system',
      eventType: 'task_created',
      payload: { type: 'task_created', status: 'queued' },
    } as const;
    const publicEventA = toPublicTaskEvent(rawEvent, publicTaskA.reference);
    const publicEventB = toPublicTaskEvent(rawEvent, publicTaskB.reference);

    expect(publicTaskA.reference.opaqueId).not.toBe(publicTaskB.reference.opaqueId);
    expect(publicEventA.reference.opaqueId).not.toBe(publicEventB.reference.opaqueId);

    revokePublicTaskReference(publicTaskA.reference);

    expect(resolvePublicTaskReference(publicTaskA.reference)).toBeNull();
    expect(resolvePublicTaskEventReference(publicEventA.reference)).toBeNull();
    expect(resolvePublicTaskReference(publicTaskB.reference)).toEqual({
      taskId: 'task-shared-owners',
    });
    expect(resolvePublicTaskEventReference(publicEventB.reference)).toEqual({
      eventId: 15,
      taskId: 'task-shared-owners',
    });

    revokePublicTaskReference(publicTaskB.reference);

    expect(resolvePublicTaskReference(publicTaskB.reference)).toBeNull();
    expect(resolvePublicTaskEventReference(publicEventB.reference)).toBeNull();
  });

  it('uses high-entropy opaque refs and rejects forged lookalikes', () => {
    const publicTask = toPublicTask(task({ taskId: 'task-secret' }));
    const publicEvent = toPublicTaskEvent(
      {
        eventId: 14,
        taskId: 'task-secret',
        taskLocalSequence: 2,
        occurredAtMs: 910,
        actor: 'system',
        eventType: 'task_status_changed',
        payload: { type: 'task_status_changed', from: 'queued', to: 'running' },
      },
      publicTask.reference
    );

    expect(publicTask.reference.opaqueId).toMatch(
      /^mcp_task_[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i
    );
    expect(publicEvent.reference.opaqueId).toMatch(
      /^mcp_task_event_[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i
    );
    expect(resolvePublicTaskReference(publicTask.reference)).toEqual({ taskId: 'task-secret' });
    expect(resolvePublicTaskEventReference(publicEvent.reference)).toEqual({
      eventId: 14,
      taskId: 'task-secret',
    });

    expect(
      resolvePublicTaskReference({
        ...publicTask.reference,
        opaqueId: mutateOpaqueId(publicTask.reference.opaqueId),
      })
    ).toBeNull();
    expect(
      resolvePublicTaskEventReference({
        ...publicEvent.reference,
        opaqueId: mutateOpaqueId(publicEvent.reference.opaqueId),
      })
    ).toBeNull();
  });

  it('expires stale references while keeping newer active references resolvable', () => {
    __setOpaqueReferenceStoreLimitsForTests({
      taskTtlMs: 10,
      eventTtlMs: 10,
      maxTasks: 16,
      maxEvents: 16,
    });

    const staleTask = toPublicTask(task({ taskId: 'task-stale' }));
    const staleEvent = toPublicTaskEvent({
      eventId: 12,
      taskId: 'task-stale',
      taskLocalSequence: 1,
      occurredAtMs: 900,
      actor: 'system',
      eventType: 'task_created',
      payload: { type: 'task_created', status: 'queued' },
    });

    now += 5;

    const freshTask = toPublicTask(task({ taskId: 'task-fresh' }));
    const freshEvent = toPublicTaskEvent({
      eventId: 13,
      taskId: 'task-fresh',
      taskLocalSequence: 1,
      occurredAtMs: 905,
      actor: 'system',
      eventType: 'task_created',
      payload: { type: 'task_created', status: 'queued' },
    });

    now += 6;

    expect(resolvePublicTaskReference(staleTask.reference)).toBeNull();
    expect(resolvePublicTaskEventReference(staleEvent.reference)).toBeNull();
    expect(resolvePublicTaskReference(freshTask.reference)).toEqual({ taskId: 'task-fresh' });
    expect(resolvePublicTaskEventReference(freshEvent.reference)).toEqual({
      eventId: 13,
      taskId: 'task-fresh',
    });
  });

  it('evicts least recently used references and cascades monitor release cleanup', () => {
    __setOpaqueReferenceStoreLimitsForTests({
      taskTtlMs: 1_000,
      eventTtlMs: 1_000,
      maxTasks: 2,
      maxEvents: 2,
    });

    const firstTask = toPublicTask(task({ taskId: 'task-1' }));
    const firstEvent = toPublicTaskEvent({
      eventId: 21,
      taskId: 'task-1',
      taskLocalSequence: 1,
      occurredAtMs: 1_000,
      actor: 'system',
      eventType: 'task_created',
      payload: { type: 'task_created', status: 'queued' },
    });

    now += 1;
    const secondTask = toPublicTask(task({ taskId: 'task-2' }));
    const secondEvent = toPublicTaskEvent({
      eventId: 22,
      taskId: 'task-2',
      taskLocalSequence: 1,
      occurredAtMs: 1_001,
      actor: 'system',
      eventType: 'task_created',
      payload: { type: 'task_created', status: 'queued' },
    });

    now += 1;
    expect(resolvePublicTaskReference(firstTask.reference)).toEqual({ taskId: 'task-1' });
    expect(resolvePublicTaskEventReference(firstEvent.reference)).toEqual({
      eventId: 21,
      taskId: 'task-1',
    });

    now += 1;
    const thirdTask = toPublicTask(task({ taskId: 'task-3' }));
    expect(resolvePublicTaskReference(secondTask.reference)).toBeNull();
    expect(resolvePublicTaskEventReference(secondEvent.reference)).toBeNull();
    expect(resolvePublicTaskReference(firstTask.reference)).toEqual({ taskId: 'task-1' });
    expect(resolvePublicTaskReference(thirdTask.reference)).toEqual({ taskId: 'task-3' });

    now += 1;
    const thirdEvent = toPublicTaskEvent({
      eventId: 23,
      taskId: 'task-3',
      taskLocalSequence: 1,
      occurredAtMs: 1_003,
      actor: 'system',
      eventType: 'task_created',
      payload: { type: 'task_created', status: 'queued' },
    });

    now += 1;
    const fourthEvent = toPublicTaskEvent({
      eventId: 24,
      taskId: 'task-3',
      taskLocalSequence: 2,
      occurredAtMs: 1_004,
      actor: 'system',
      eventType: 'task_status_changed',
      payload: { type: 'task_status_changed', from: 'queued', to: 'running' },
    });

    expect(resolvePublicTaskEventReference(firstEvent.reference)).toBeNull();
    expect(resolvePublicTaskEventReference(thirdEvent.reference)).toEqual({
      eventId: 23,
      taskId: 'task-3',
    });
    expect(resolvePublicTaskEventReference(fourthEvent.reference)).toEqual({
      eventId: 24,
      taskId: 'task-3',
    });

    const cursor = createMcpTaskMonitorCursor(
      { taskId: 'task-3', revision: thirdTask.revision },
      thirdTask.reference
    );
    releaseMcpTaskMonitorCursor(cursor);

    expect(resolvePublicTaskReference(thirdTask.reference)).toBeNull();
    expect(resolvePublicTaskEventReference(thirdEvent.reference)).toBeNull();
    expect(resolvePublicTaskEventReference(fourthEvent.reference)).toBeNull();
  });

  it('keeps active cursor refs alive across TTL/LRU pruning and invalidates them on release', () => {
    __setOpaqueReferenceStoreLimitsForTests({
      taskTtlMs: 5,
      eventTtlMs: 5,
      maxTasks: 1,
      maxEvents: 1,
    });

    const initialRawTask = task({ taskId: 'task-lived', revision: 1, status: 'running' });
    const initialPublicTask = toPublicTask(initialRawTask);
    const cursor = createMcpTaskMonitorCursor(initialRawTask);

    const firstMerge = mergeMcpTaskMonitorPage(
      cursor,
      initialPublicTask,
      [],
      initialRawTask,
      events([
        {
          eventId: 31,
          taskId: 'task-lived',
          taskLocalSequence: 1,
          occurredAtMs: 1_100,
          actor: 'system',
          eventType: 'task_created',
          payload: { type: 'task_created', status: 'queued' },
        },
      ])
    );

    now += 10;
    toPublicTask(task({ taskId: 'task-evicted', revision: 1 }));
    toPublicTaskEvent({
      eventId: 32,
      taskId: 'task-evicted',
      taskLocalSequence: 1,
      occurredAtMs: 1_110,
      actor: 'system',
      eventType: 'task_created',
      payload: { type: 'task_created', status: 'queued' },
    });

    expect(resolvePublicTaskReference(firstMerge.task.reference)).toEqual({ taskId: 'task-lived' });
    expect(resolvePublicTaskEventReference(firstMerge.events[0]!.reference)).toEqual({
      eventId: 31,
      taskId: 'task-lived',
    });

    const secondMerge = mergeMcpTaskMonitorPage(
      firstMerge.cursor,
      firstMerge.task,
      firstMerge.events,
      task({ taskId: 'task-lived', revision: 2, status: 'running' }),
      events([
        {
          eventId: 31,
          taskId: 'task-lived',
          taskLocalSequence: 1,
          occurredAtMs: 1_100,
          actor: 'system',
          eventType: 'task_created',
          payload: { type: 'task_created', status: 'queued' },
        },
        {
          eventId: 33,
          taskId: 'task-lived',
          taskLocalSequence: 2,
          occurredAtMs: 1_111,
          actor: 'system',
          eventType: 'task_status_changed',
          payload: { type: 'task_status_changed', from: 'queued', to: 'running' },
        },
      ])
    );

    expect(secondMerge.events.map((event) => event.kind)).toEqual(['created', 'status_changed']);
    expect(secondMerge.events).toHaveLength(2);
    expect(resolvePublicTaskReference(secondMerge.task.reference)).toEqual({
      taskId: 'task-lived',
    });

    releaseMcpTaskMonitorCursor(secondMerge.cursor);

    expect(getMcpTaskMonitorResumeRequest(secondMerge.cursor)).toBeNull();
    expect(resolvePublicTaskReference(secondMerge.task.reference)).toBeNull();
    expect(resolvePublicTaskEventReference(secondMerge.events[0]!.reference)).toBeNull();
    expect(resolvePublicTaskEventReference(secondMerge.events[1]!.reference)).toBeNull();
  });

  it('keeps same-task cursors isolated so releasing one cursor leaves the sibling resumable', () => {
    const sharedRawTask = task({ taskId: 'task-shared-cursors', revision: 1, status: 'running' });
    const rawCreatedEvent = {
      eventId: 61,
      taskId: 'task-shared-cursors',
      taskLocalSequence: 1,
      occurredAtMs: 1_300,
      actor: 'system',
      eventType: 'task_created',
      payload: { type: 'task_created', status: 'queued' },
    } as const;

    const cursorA = createMcpTaskMonitorCursor(sharedRawTask);
    const cursorB = createMcpTaskMonitorCursor(sharedRawTask);

    const firstMergeA = mergeMcpTaskMonitorPage(
      cursorA,
      toPublicTask(sharedRawTask),
      [],
      sharedRawTask,
      events([rawCreatedEvent])
    );
    const firstMergeB = mergeMcpTaskMonitorPage(
      cursorB,
      toPublicTask(sharedRawTask),
      [],
      sharedRawTask,
      events([rawCreatedEvent])
    );

    expect(firstMergeA.task.reference.opaqueId).not.toBe(firstMergeB.task.reference.opaqueId);
    expect(firstMergeA.events[0]!.reference.opaqueId).not.toBe(
      firstMergeB.events[0]!.reference.opaqueId
    );

    releaseMcpTaskMonitorCursor(firstMergeA.cursor);

    expect(getMcpTaskMonitorResumeRequest(firstMergeA.cursor)).toBeNull();
    expect(resolvePublicTaskReference(firstMergeA.task.reference)).toBeNull();
    expect(resolvePublicTaskEventReference(firstMergeA.events[0]!.reference)).toBeNull();

    expect(getMcpTaskMonitorResumeRequest(firstMergeB.cursor)).toEqual({
      afterEventId: 9,
      taskIds: ['task-shared-cursors'],
    });
    expect(resolvePublicTaskReference(firstMergeB.task.reference)).toEqual({
      taskId: 'task-shared-cursors',
    });
    expect(resolvePublicTaskEventReference(firstMergeB.events[0]!.reference)).toEqual({
      eventId: 61,
      taskId: 'task-shared-cursors',
    });

    const secondMergeB = mergeMcpTaskMonitorPage(
      firstMergeB.cursor,
      firstMergeB.task,
      firstMergeB.events,
      task({ taskId: 'task-shared-cursors', revision: 2, status: 'running' }),
      events([
        rawCreatedEvent,
        {
          eventId: 62,
          taskId: 'task-shared-cursors',
          taskLocalSequence: 2,
          occurredAtMs: 1_301,
          actor: 'system',
          eventType: 'task_status_changed',
          payload: { type: 'task_status_changed', from: 'queued', to: 'running' },
        },
      ])
    );

    expect(secondMergeB.events).toHaveLength(2);
    expect(resolvePublicTaskEventReference(secondMergeB.events[1]!.reference)).toEqual({
      eventId: 62,
      taskId: 'task-shared-cursors',
    });

    releaseMcpTaskMonitorCursor(secondMergeB.cursor);

    expect(getMcpTaskMonitorResumeRequest(secondMergeB.cursor)).toBeNull();
    expect(resolvePublicTaskReference(secondMergeB.task.reference)).toBeNull();
    expect(resolvePublicTaskEventReference(secondMergeB.events[0]!.reference)).toBeNull();
    expect(resolvePublicTaskEventReference(secondMergeB.events[1]!.reference)).toBeNull();
  });

  it('atomically rebuilds cursor refs when the backing store disappears before resume merge', () => {
    const initialRawTask = task({ taskId: 'task-rebuild', revision: 1, status: 'running' });
    const initialPublicTask = toPublicTask(initialRawTask);
    const cursor = createMcpTaskMonitorCursor(initialRawTask);

    const firstMerge = mergeMcpTaskMonitorPage(
      cursor,
      initialPublicTask,
      [],
      initialRawTask,
      events([
        {
          eventId: 41,
          taskId: 'task-rebuild',
          taskLocalSequence: 1,
          occurredAtMs: 1_200,
          actor: 'system',
          eventType: 'task_created',
          payload: { type: 'task_created', status: 'queued' },
        },
      ])
    );

    const oldEventRef = firstMerge.events[0]!.reference.opaqueId;
    revokePublicTaskReference(firstMerge.task.reference);
    expect(resolvePublicTaskReference(firstMerge.task.reference)).toBeNull();
    expect(resolvePublicTaskEventReference(firstMerge.events[0]!.reference)).toBeNull();

    const secondMerge = mergeMcpTaskMonitorPage(
      firstMerge.cursor,
      firstMerge.task,
      firstMerge.events,
      task({ taskId: 'task-rebuild', revision: 2, status: 'running' }),
      events([
        {
          eventId: 41,
          taskId: 'task-rebuild',
          taskLocalSequence: 1,
          occurredAtMs: 1_200,
          actor: 'system',
          eventType: 'task_created',
          payload: { type: 'task_created', status: 'queued' },
        },
        {
          eventId: 42,
          taskId: 'task-rebuild',
          taskLocalSequence: 2,
          occurredAtMs: 1_201,
          actor: 'system',
          eventType: 'task_status_changed',
          payload: { type: 'task_status_changed', from: 'queued', to: 'running' },
        },
      ])
    );

    expect(secondMerge.taskChanged).toBe(true);
    expect(secondMerge.events.map((event) => event.kind)).toEqual(['created', 'status_changed']);
    expect(secondMerge.events[0]!.reference.opaqueId).not.toBe(oldEventRef);
    expect(resolvePublicTaskReference(secondMerge.task.reference)).toEqual({
      taskId: 'task-rebuild',
    });
    expect(resolvePublicTaskEventReference(secondMerge.events[0]!.reference)).toEqual({
      eventId: 41,
      taskId: 'task-rebuild',
    });
    expect(resolvePublicTaskEventReference(secondMerge.events[1]!.reference)).toEqual({
      eventId: 42,
      taskId: 'task-rebuild',
    });
  });

  it('never derives support refs from raw messages when correlationId is missing', () => {
    const rawMessage =
      'Authorization: Bearer top-secret token=abc123 path=/tmp/private ' +
      'C:\\\\sensitive\\\\token.txt https://private.example.test/query?token=abc123';

    const safeError = toMcpCenterSafeError({
      code: 'adapter_failed',
      message: rawMessage,
      retryable: false,
    });
    const unknownError = toMcpCenterSafeError(new Error(rawMessage));

    for (const error of [safeError, unknownError]) {
      expect(error.supportRef).toMatch(/^MCP-(?:[0-9A-F]{32}|UNAVAILABLE)$/);
      expect(error.supportRef).not.toContain('top-secret');
      expect(error.supportRef).not.toContain('abc123');
      expect(error.supportRef).not.toContain('/tmp/private');
      expect(error.supportRef).not.toContain('sensitive');
    }

    expect(safeError.supportRef).not.toBe(legacyHashedSupportRef('adapter_failed', rawMessage));
    expect(unknownError.supportRef).not.toBe(legacyHashedSupportRef('unknown', rawMessage));
  });

  it('keeps forbidden raw fields out of public type surfaces', () => {
    type PublicTaskKeys = keyof PublicTask;
    type PublicTaskEventKeys = keyof PublicTaskEvent;
    type SafeErrorKeys = keyof McpCenterSafeError;

    expectTypeOf<Extract<'taskId', PublicTaskKeys>>().toEqualTypeOf<never>();
    expectTypeOf<Extract<'payload', PublicTaskEventKeys>>().toEqualTypeOf<never>();
    expectTypeOf<Extract<'correlationId', SafeErrorKeys>>().toEqualTypeOf<never>();
    expectTypeOf<Extract<'message', SafeErrorKeys>>().toEqualTypeOf<never>();
  });
});
