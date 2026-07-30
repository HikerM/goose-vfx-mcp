import type { McpEventsPage, McpTaskRef } from '@aaif/goose-sdk';
import type {
  McpCenterSafeError,
  McpCenterSafeErrorCode,
  McpCenterSafeErrorUserCopyKey,
  PublicTask,
  PublicTaskCapabilities,
  PublicTaskCapabilityState,
  PublicTaskDisplayState,
  PublicTaskEvent,
  PublicTaskEventReference,
  PublicTaskEventDisplayState,
  PublicTaskEventKind,
  PublicTaskMutationTarget,
  PublicTaskNextAction,
  PublicTaskOperation,
  PublicTaskOutcomeState,
  PublicTaskRecoveryDecision,
  PublicTaskReference,
  PublicTaskRemainingEffect,
  PublicTaskRollbackState,
  PublicTaskStatus,
  PublicTaskStepState,
  PublicTaskFinalizationState,
} from '../public-types';

type RawTaskEvent = Pick<
  McpEventsPage['events'][number],
  'eventId' | 'taskId' | 'taskLocalSequence' | 'occurredAtMs' | 'actor'
> & {
  eventType: string;
  payload: unknown;
};

type OrderedPublicTaskEvent = {
  event: PublicTaskEvent;
  order: number;
};

type RawTaskResolution = {
  taskId: string;
  revision: number;
};

type TaskReferenceEntry = RawTaskResolution & {
  opaqueId: string;
  eventOpaqueIds: Set<string>;
  expiresAtMs: number;
  lastTouchedAtMs: number;
  leaseCount: number;
};

type EventReferenceEntry = {
  opaqueId: string;
  eventId: number;
  taskId: string;
  taskOpaqueId: string;
  expiresAtMs: number;
  lastTouchedAtMs: number;
  leaseCount: number;
};

type InternalTaskMonitorCursor = {
  afterEventId?: number;
  eventOrder: Readonly<Record<string, number>>;
  leasedEventOpaqueIds: readonly string[];
  latestRevision: number;
  rawTaskId: string;
  taskReference: PublicTaskReference;
};

declare const taskMonitorCursorBrand: unique symbol;

export type McpTaskMonitorCursor = {
  readonly [taskMonitorCursorBrand]: 'McpTaskMonitorCursor';
};

export type McpTaskMonitorMergeResult = {
  cursor: McpTaskMonitorCursor;
  events: PublicTaskEvent[];
  task: PublicTask;
  taskChanged: boolean;
  terminal: boolean;
};

const knownTaskOperations = new Set<PublicTaskOperation>([
  'register',
  'install',
  'update',
  'repair',
  'uninstall',
  'health',
]);

const knownTaskStatuses = new Set<PublicTaskStatus>([
  'planned',
  'awaiting_confirmation',
  'queued',
  'running',
  'cancelling',
  'verifying',
  'activating',
  'rolling_back',
  'succeeded',
  'failed',
  'cancelled',
  'interrupted',
  'recovery_required',
]);

const knownOutcomeStates = new Set<PublicTaskOutcomeState>([
  'pending',
  'succeeded',
  'failed',
  'cancelled',
  'interrupted',
  'recovery_required',
]);

const knownRollbackStates = new Set<PublicTaskRollbackState>([
  'not_required',
  'pending',
  'in_progress',
  'complete',
  'incomplete',
]);

const knownFinalizationStates = new Set<PublicTaskFinalizationState>([
  'pending',
  'complete',
  'recovery_required',
]);

const knownRemainingEffects = new Set<PublicTaskRemainingEffect>([
  'connection_projection',
  'managed_installation',
  'managed_uninstall',
  'external_resource',
]);

const knownSafeErrorCodes = new Set<McpCenterSafeErrorCode>([
  'invalid_request',
  'unsafe_url',
  'not_found',
  'integrity_error',
  'policy_denied',
  'not_implemented_for_phase',
  'operation_not_supported',
  'manual_stdio_provider_unavailable',
  'remote_http_policy_unavailable',
  'plan_stale',
  'plan_expired',
  'idempotency_conflict',
  'revision_conflict',
  'invalid_transition',
  'repository_unavailable',
  'projection_conflict',
  'credential_missing',
  'health_failed',
  'task_not_cancellable',
  'rollback_incomplete',
  'adapter_incompatible',
  'docker_unavailable',
  'daemon_policy_denied',
  'image_digest_mismatch',
  'registry_auth_required',
  'mount_permission_denied',
  'git_unavailable',
  'git_origin_denied',
  'commit_unavailable',
  'unsafe_repository_tree',
  'development_mode_required',
  'adapter_failed',
  'verification_failed',
  'activation_failed',
  'rollback_failed',
  'cancelled',
  'interrupted',
  'connection_unavailable',
  'monitor_paused',
  'unknown',
]);

const terminalTaskStatuses = new Set<PublicTaskStatus>([
  'succeeded',
  'failed',
  'cancelled',
  'interrupted',
  'recovery_required',
]);

const connectionErrorPattern =
  /(?:network|timeout|timed out|fetch|socket|econn|connection|offline|unreachable)/i;

const DEFAULT_TASK_REFERENCE_TTL_MS = 30 * 60 * 1000;
const DEFAULT_EVENT_REFERENCE_TTL_MS = 30 * 60 * 1000;
const DEFAULT_MAX_TASK_REFERENCES = 256;
const DEFAULT_MAX_EVENT_REFERENCES = 2048;
const OPAQUE_REFERENCE_CORRELATION_ID = 'mcp-task-reference';

const taskReferenceByOpaqueId = new Map<string, TaskReferenceEntry>();
const taskOpaqueIdsByRawTaskId = new Map<string, Set<string>>();
const eventReferenceByOpaqueId = new Map<string, EventReferenceEntry>();
const opaqueEventIdByScopedRawKey = new Map<string, string>();
// Keep raw resume cursors and event ordering private to the adapter boundary.
let taskMonitorCursorState = new WeakMap<McpTaskMonitorCursor, InternalTaskMonitorCursor>();
let referenceStoreNow = () => Date.now();
let taskReferenceTtlMs = DEFAULT_TASK_REFERENCE_TTL_MS;
let eventReferenceTtlMs = DEFAULT_EVENT_REFERENCE_TTL_MS;
let maxTaskReferences = DEFAULT_MAX_TASK_REFERENCES;
let maxEventReferences = DEFAULT_MAX_EVENT_REFERENCES;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function readString(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

function readBoolean(value: unknown): boolean | null {
  return typeof value === 'boolean' ? value : null;
}

function readNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function createOpaqueReferenceError(): Error & {
  code: McpCenterSafeErrorCode;
  correlationId: string;
  retryable: false;
} {
  const error = new Error('Secure opaque reference generation is unavailable.');
  return Object.assign(error, {
    code: 'adapter_incompatible' as const,
    correlationId: OPAQUE_REFERENCE_CORRELATION_ID,
    retryable: false as const,
  });
}

function bytesToUuid(bytes: Uint8Array): string {
  const hex = Array.from(bytes, (value) => value.toString(16).padStart(2, '0'));
  return [
    hex.slice(0, 4).join(''),
    hex.slice(4, 6).join(''),
    hex.slice(6, 8).join(''),
    hex.slice(8, 10).join(''),
    hex.slice(10, 16).join(''),
  ].join('-');
}

function createSecureUuid(): string {
  const runtimeCrypto = globalThis.crypto;
  if (runtimeCrypto && typeof runtimeCrypto.randomUUID === 'function') {
    return runtimeCrypto.randomUUID();
  }
  if (runtimeCrypto && typeof runtimeCrypto.getRandomValues === 'function') {
    const bytes = new Uint8Array(16);
    runtimeCrypto.getRandomValues(bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    return bytesToUuid(bytes);
  }
  throw createOpaqueReferenceError();
}

function createOpaqueId(prefix: string): string {
  return `${prefix}_${createSecureUuid()}`;
}

function nowMs(): number {
  return referenceStoreNow();
}

function hasActiveLease(leaseCount: number): boolean {
  return leaseCount > 0;
}

function touchTaskReferenceEntry(entry: TaskReferenceEntry) {
  const currentNow = nowMs();
  entry.lastTouchedAtMs = currentNow;
  entry.expiresAtMs = currentNow + taskReferenceTtlMs;
}

function touchEventReferenceEntry(entry: EventReferenceEntry) {
  const currentNow = nowMs();
  entry.lastTouchedAtMs = currentNow;
  entry.expiresAtMs = currentNow + eventReferenceTtlMs;
}

function deleteEventReferenceEntry(opaqueId: string) {
  const entry = eventReferenceByOpaqueId.get(opaqueId);
  if (!entry) return;
  eventReferenceByOpaqueId.delete(opaqueId);
  opaqueEventIdByScopedRawKey.delete(`${entry.taskOpaqueId}:${entry.eventId}`);
  taskReferenceByOpaqueId.get(entry.taskOpaqueId)?.eventOpaqueIds.delete(opaqueId);
}

function deleteTaskReferenceEntry(opaqueId: string) {
  const entry = taskReferenceByOpaqueId.get(opaqueId);
  if (!entry) return;
  taskReferenceByOpaqueId.delete(opaqueId);
  const trackedOpaqueIds = taskOpaqueIdsByRawTaskId.get(entry.taskId);
  if (trackedOpaqueIds) {
    trackedOpaqueIds.delete(opaqueId);
    if (trackedOpaqueIds.size === 0) taskOpaqueIdsByRawTaskId.delete(entry.taskId);
  }
  for (const eventOpaqueId of Array.from(entry.eventOpaqueIds)) {
    deleteEventReferenceEntry(eventOpaqueId);
  }
}

function leaseTaskReferenceEntry(entry: TaskReferenceEntry) {
  entry.leaseCount += 1;
}

function leaseEventReferenceEntry(entry: EventReferenceEntry) {
  entry.leaseCount += 1;
}

function releaseTaskReferenceLease(opaqueId: string) {
  const entry = taskReferenceByOpaqueId.get(opaqueId);
  if (!entry) return;
  entry.leaseCount = Math.max(0, entry.leaseCount - 1);
}

function releaseEventReferenceLease(opaqueId: string) {
  const entry = eventReferenceByOpaqueId.get(opaqueId);
  if (!entry) return;
  entry.leaseCount = Math.max(0, entry.leaseCount - 1);
}

function selectLeastRecentlyUsedOpaqueId<T extends { lastTouchedAtMs: number; opaqueId: string }>(
  entries: Iterable<T>
): string | null {
  let oldest: T | null = null;
  for (const entry of entries) {
    if (!oldest || entry.lastTouchedAtMs < oldest.lastTouchedAtMs) oldest = entry;
  }
  return oldest?.opaqueId ?? null;
}

function pruneExpiredReferenceEntries() {
  const currentNow = nowMs();
  for (const entry of Array.from(taskReferenceByOpaqueId.values())) {
    if (hasActiveLease(entry.leaseCount)) continue;
    if (entry.expiresAtMs <= currentNow) deleteTaskReferenceEntry(entry.opaqueId);
  }
  for (const entry of Array.from(eventReferenceByOpaqueId.values())) {
    if (hasActiveLease(entry.leaseCount)) continue;
    if (entry.expiresAtMs <= currentNow) deleteEventReferenceEntry(entry.opaqueId);
  }
}

function pruneReferenceCapacity() {
  while (taskReferenceByOpaqueId.size > maxTaskReferences) {
    const oldestOpaqueId = selectLeastRecentlyUsedOpaqueId(
      Array.from(taskReferenceByOpaqueId.values()).filter(
        (entry) => !hasActiveLease(entry.leaseCount)
      )
    );
    if (!oldestOpaqueId) break;
    deleteTaskReferenceEntry(oldestOpaqueId);
  }
  while (eventReferenceByOpaqueId.size > maxEventReferences) {
    const oldestOpaqueId = selectLeastRecentlyUsedOpaqueId(
      Array.from(eventReferenceByOpaqueId.values()).filter(
        (entry) => !hasActiveLease(entry.leaseCount)
      )
    );
    if (!oldestOpaqueId) break;
    deleteEventReferenceEntry(oldestOpaqueId);
  }
}

function pruneReferenceStore() {
  pruneExpiredReferenceEntries();
  pruneReferenceCapacity();
}

function getTaskReferenceEntry(
  opaqueId: string,
  { touch = false }: { touch?: boolean } = {}
): TaskReferenceEntry | null {
  pruneReferenceStore();
  const entry = taskReferenceByOpaqueId.get(opaqueId);
  if (!entry) return null;
  if (touch) touchTaskReferenceEntry(entry);
  return entry;
}

function findMostRecentlyTouchedTaskReference(taskId: string): PublicTaskReference | null {
  pruneReferenceStore();
  const opaqueIds = taskOpaqueIdsByRawTaskId.get(taskId);
  if (!opaqueIds) return null;

  let newest: TaskReferenceEntry | null = null;
  for (const opaqueId of opaqueIds) {
    const entry = taskReferenceByOpaqueId.get(opaqueId);
    if (!entry || entry.taskId !== taskId) continue;
    if (!newest || entry.lastTouchedAtMs > newest.lastTouchedAtMs) newest = entry;
  }
  if (!newest) return null;
  touchTaskReferenceEntry(newest);
  return { kind: 'mcp_task', opaqueId: newest.opaqueId };
}

function getEventReferenceEntry(
  opaqueId: string,
  { touch = false }: { touch?: boolean } = {}
): EventReferenceEntry | null {
  pruneReferenceStore();
  const entry = eventReferenceByOpaqueId.get(opaqueId);
  if (!entry) return null;
  if (!taskReferenceByOpaqueId.has(entry.taskOpaqueId)) {
    deleteEventReferenceEntry(opaqueId);
    return null;
  }
  if (touch) {
    touchEventReferenceEntry(entry);
    const taskEntry = taskReferenceByOpaqueId.get(entry.taskOpaqueId);
    if (taskEntry) touchTaskReferenceEntry(taskEntry);
  }
  return entry;
}

function rememberTaskReference(
  taskId: string,
  revision?: number,
  existingReference?: PublicTaskReference
): PublicTaskReference {
  pruneReferenceStore();
  const reusableOpaqueId = existingReference?.opaqueId;
  let current =
    reusableOpaqueId && taskReferenceByOpaqueId.get(reusableOpaqueId)?.taskId === taskId
      ? taskReferenceByOpaqueId.get(reusableOpaqueId)
      : undefined;
  if (!current) {
    const opaqueId = createOpaqueId('mcp_task');
    current = {
      opaqueId,
      taskId,
      revision: typeof revision === 'number' ? revision : 0,
      eventOpaqueIds: new Set(),
      expiresAtMs: 0,
      lastTouchedAtMs: 0,
      leaseCount: 0,
    };
    taskReferenceByOpaqueId.set(opaqueId, current);
    let trackedOpaqueIds = taskOpaqueIdsByRawTaskId.get(taskId);
    if (!trackedOpaqueIds) {
      trackedOpaqueIds = new Set();
      taskOpaqueIdsByRawTaskId.set(taskId, trackedOpaqueIds);
    }
    trackedOpaqueIds.add(opaqueId);
  }
  if (typeof revision === 'number') current.revision = Math.max(current.revision, revision);
  touchTaskReferenceEntry(current);
  return { kind: 'mcp_task', opaqueId: current.opaqueId };
}

function rememberEventReference(taskReference: PublicTaskReference, eventId: number) {
  const taskEntry = taskReferenceByOpaqueId.get(taskReference.opaqueId);
  if (!taskEntry) {
    throw createOpaqueReferenceError();
  }
  touchTaskReferenceEntry(taskEntry);
  const rawKey = `${taskEntry.opaqueId}:${eventId}`;
  let opaqueId = opaqueEventIdByScopedRawKey.get(rawKey);
  let current = opaqueId ? eventReferenceByOpaqueId.get(opaqueId) : undefined;
  if (opaqueId && !current) {
    opaqueEventIdByScopedRawKey.delete(rawKey);
    opaqueId = undefined;
  }
  if (!opaqueId || !current) {
    opaqueId = createOpaqueId('mcp_task_event');
    current = {
      opaqueId,
      eventId,
      taskId: taskEntry.taskId,
      taskOpaqueId: taskEntry.opaqueId,
      expiresAtMs: 0,
      lastTouchedAtMs: 0,
      leaseCount: 0,
    };
    eventReferenceByOpaqueId.set(opaqueId, current);
    opaqueEventIdByScopedRawKey.set(rawKey, opaqueId);
    taskEntry.eventOpaqueIds.add(opaqueId);
  }
  touchEventReferenceEntry(current);
  return { kind: 'mcp_task_event' as const, opaqueId: current.opaqueId };
}

function clampProgress(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(100, Math.round(value)));
}

function hashTrustedCorrelationId(value: string): string {
  let hash = 0x811c9dc5;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, '0');
}

function createOpaqueSupportRef(): string {
  try {
    return `MCP-${createSecureUuid().replace(/-/g, '').toUpperCase()}`;
  } catch {
    return 'MCP-UNAVAILABLE';
  }
}

function createSupportRef(code: McpCenterSafeErrorCode, correlationId: string | null): string {
  if (!correlationId) return createOpaqueSupportRef();
  return `MCP-${hashTrustedCorrelationId(`${code}:${correlationId}`).toUpperCase()}`;
}

function toUserCopyKey(code: McpCenterSafeErrorCode): McpCenterSafeErrorUserCopyKey {
  return code === 'unknown' ? 'mcpCenter.safeError.generic' : `mcpCenter.safeError.${code}`;
}

function normalizeSafeErrorCode(value: string | null): McpCenterSafeErrorCode {
  if (!value) return 'unknown';
  return knownSafeErrorCodes.has(value as McpCenterSafeErrorCode)
    ? (value as McpCenterSafeErrorCode)
    : 'unknown';
}

function readErrorEnvelope(value: unknown): Record<string, unknown> | null {
  if (!isRecord(value)) return null;
  if (isRecord(value.envelope)) return value.envelope;
  if (typeof value.code === 'string' || typeof value.correlationId === 'string') return value;
  return null;
}

function readErrorMessage(value: unknown): string | null {
  if (!isRecord(value)) return value instanceof Error ? value.message : null;
  if (typeof value.message === 'string') return value.message;
  if (isRecord(value.envelope) && typeof value.envelope.message === 'string') {
    return value.envelope.message;
  }
  return value instanceof Error ? value.message : null;
}

function isConnectionErrorMessage(message: string | null): boolean {
  return !!message && connectionErrorPattern.test(message);
}

function taskStatusToDisplayState(
  status: PublicTaskStatus,
  nextAction?: PublicTaskNextAction
): PublicTaskDisplayState {
  switch (status) {
    case 'planned':
    case 'queued':
    case 'running':
    case 'cancelling':
    case 'verifying':
    case 'activating':
    case 'rolling_back':
      return nextAction === 'cancel_when_safe' ? 'paused' : 'active';
    case 'awaiting_confirmation':
    case 'interrupted':
      return 'paused';
    case 'recovery_required':
      return 'recovery';
    case 'succeeded':
      return 'succeeded';
    case 'failed':
      return 'failed';
    case 'cancelled':
      return 'cancelled';
    case 'unknown':
      return nextAction === 'recovery'
        ? 'recovery'
        : nextAction === 'wait' || nextAction === 'cancel_when_safe'
          ? 'paused'
          : 'unknown';
  }
}

function taskStatusToEventDisplayState(status: PublicTaskStatus): PublicTaskEventDisplayState {
  switch (taskStatusToDisplayState(status)) {
    case 'active':
      return 'active';
    case 'paused':
      return 'paused';
    case 'recovery':
      return 'recovery';
    case 'succeeded':
      return 'succeeded';
    case 'failed':
      return 'failed';
    case 'cancelled':
      return 'cancelled';
    case 'unknown':
      return 'unknown';
  }
}

function normalizeStepState(value: string | null): PublicTaskStepState {
  if (value === 'not_started' || value === 'started' || value === 'committed') return value;
  return 'unknown';
}

function stepStateToDisplayState(state: PublicTaskStepState): PublicTaskEventDisplayState {
  switch (state) {
    case 'not_started':
    case 'started':
      return 'active';
    case 'committed':
      return 'succeeded';
    case 'unknown':
      return 'unknown';
  }
}

function normalizeRecoveryDecision(value: string | null): PublicTaskRecoveryDecision {
  if (value === 'resume_from_step') return 'resume';
  if (value === 'rollback_from_step') return 'rollback';
  if (value === 'requires_manual_recovery') return 'manual_recovery';
  return value ? 'recovery' : 'unknown';
}

function createTaskCapabilities(
  status: PublicTaskStatus,
  nextAction: PublicTaskNextAction,
  cancellable: boolean
): PublicTaskCapabilities {
  const cancel: PublicTaskCapabilityState = cancellable
    ? 'available'
    : status === 'unknown'
      ? 'unknown'
      : nextAction === 'cancel_when_safe'
        ? 'paused'
        : 'unavailable';

  const retry: PublicTaskCapabilityState =
    nextAction === 'retry'
      ? 'available'
      : nextAction === 'recovery'
        ? 'recovery'
        : status === 'interrupted'
          ? 'paused'
          : status === 'unknown'
            ? 'unknown'
            : 'unavailable';

  const review: PublicTaskCapabilityState =
    status === 'awaiting_confirmation'
      ? 'available'
      : status === 'recovery_required' || nextAction === 'recovery'
        ? 'recovery'
        : status === 'interrupted'
          ? 'paused'
          : status === 'unknown'
            ? 'unknown'
            : 'unavailable';

  return { cancel, retry, review };
}

function createCursor(state: InternalTaskMonitorCursor): McpTaskMonitorCursor {
  const cursor = {} as McpTaskMonitorCursor;
  setCursorState(cursor, state);
  return cursor;
}

function readCursor(cursor: McpTaskMonitorCursor): InternalTaskMonitorCursor | null {
  return taskMonitorCursorState.get(cursor) ?? null;
}

function applyCursorLeases(state: InternalTaskMonitorCursor) {
  const taskEntry = taskReferenceByOpaqueId.get(state.taskReference.opaqueId);
  if (taskEntry) {
    touchTaskReferenceEntry(taskEntry);
    leaseTaskReferenceEntry(taskEntry);
  }
  for (const opaqueId of state.leasedEventOpaqueIds) {
    const eventEntry = eventReferenceByOpaqueId.get(opaqueId);
    if (!eventEntry || !taskReferenceByOpaqueId.has(eventEntry.taskOpaqueId)) continue;
    const scopedTaskEntry = taskEntry ?? taskReferenceByOpaqueId.get(eventEntry.taskOpaqueId);
    if (!scopedTaskEntry) continue;
    touchEventReferenceEntry(eventEntry);
    touchTaskReferenceEntry(scopedTaskEntry);
    leaseEventReferenceEntry(eventEntry);
  }
}

function releaseCursorLeases(state: InternalTaskMonitorCursor) {
  for (const opaqueId of state.leasedEventOpaqueIds) {
    releaseEventReferenceLease(opaqueId);
  }
  releaseTaskReferenceLease(state.taskReference.opaqueId);
}

function setCursorState(
  cursor: McpTaskMonitorCursor,
  state: InternalTaskMonitorCursor
): McpTaskMonitorCursor {
  const previous = readCursor(cursor);
  if (previous) releaseCursorLeases(previous);
  const nextState: InternalTaskMonitorCursor = {
    ...state,
    leasedEventOpaqueIds: [...state.leasedEventOpaqueIds],
  };
  taskMonitorCursorState.set(cursor, nextState);
  applyCursorLeases(nextState);
  pruneReferenceStore();
  return cursor;
}

function selectNewerTask(
  taskId: string,
  currentRevision: number,
  candidates: ReadonlyArray<McpTaskRef | null | undefined>
): McpTaskRef | null {
  let winner: McpTaskRef | null = null;
  for (const candidate of candidates) {
    if (!candidate || candidate.taskId !== taskId || typeof candidate.revision !== 'number')
      continue;
    if (!winner || candidate.revision > winner.revision) winner = candidate;
  }
  return winner && winner.revision > currentRevision ? winner : null;
}

function mergeOrderedEvents(
  currentEvents: readonly PublicTaskEvent[],
  currentOrder: Readonly<Record<string, number>>,
  incomingEvents: readonly RawTaskEvent[],
  taskReference: PublicTaskReference,
  rawTaskId: string,
  maxEvents: number
) {
  const merged = new Map<string, OrderedPublicTaskEvent>();

  for (const event of currentEvents) {
    const order = currentOrder[event.reference.opaqueId];
    if (typeof order !== 'number') continue;
    merged.set(event.reference.opaqueId, { event, order });
  }

  for (const rawEvent of incomingEvents) {
    if (rawEvent.taskId !== rawTaskId) continue;
    const event = toPublicTaskEvent(rawEvent, taskReference);
    merged.set(event.reference.opaqueId, { event, order: rawEvent.eventId });
  }

  const items = Array.from(merged.values())
    .sort((left, right) => left.order - right.order)
    .slice(-maxEvents);

  const nextOrder: Record<string, number> = {};
  for (const item of items) nextOrder[item.event.reference.opaqueId] = item.order;

  return {
    eventOrder: nextOrder,
    events: items.map((item) => item.event),
  };
}

function canReuseCursorState(
  state: InternalTaskMonitorCursor,
  currentEvents: readonly PublicTaskEvent[]
): boolean {
  const taskEntry = getTaskReferenceEntry(state.taskReference.opaqueId, { touch: true });
  if (!taskEntry || taskEntry.taskId !== state.rawTaskId) return false;
  for (const event of currentEvents) {
    const eventEntry = getEventReferenceEntry(event.reference.opaqueId, { touch: true });
    if (!eventEntry || eventEntry.taskOpaqueId !== state.taskReference.opaqueId) return false;
  }
  return true;
}

function rebuildMonitorCursorState(
  cursor: McpTaskMonitorCursor,
  latestTask: McpTaskRef,
  page: McpEventsPage,
  maxEvents: number,
  referenceHint?: PublicTaskReference
): McpTaskMonitorMergeResult {
  const newerTask = selectNewerTask(latestTask.taskId, latestTask.revision, page.tasks);
  const nextTask = toPublicTask(newerTask ?? latestTask, referenceHint);
  const mergedEvents = mergeOrderedEvents(
    [],
    {},
    page.events,
    nextTask.reference,
    latestTask.taskId,
    maxEvents
  );
  setCursorState(cursor, {
    afterEventId: page.nextEventId,
    eventOrder: mergedEvents.eventOrder,
    leasedEventOpaqueIds: mergedEvents.events.map((event) => event.reference.opaqueId),
    latestRevision: nextTask.revision,
    rawTaskId: latestTask.taskId,
    taskReference: nextTask.reference,
  });
  return {
    cursor,
    events: mergedEvents.events,
    task: nextTask,
    taskChanged: true,
    terminal: terminalTaskStatuses.has(nextTask.status),
  };
}

export function normalizePublicTaskOperation(value: string): PublicTaskOperation {
  return knownTaskOperations.has(value as PublicTaskOperation)
    ? (value as PublicTaskOperation)
    : 'unknown';
}

export function normalizePublicTaskStatus(value: string): PublicTaskStatus {
  return knownTaskStatuses.has(value as PublicTaskStatus) ? (value as PublicTaskStatus) : 'unknown';
}

export function normalizePublicTaskOutcomeState(value: string): PublicTaskOutcomeState {
  return knownOutcomeStates.has(value as PublicTaskOutcomeState)
    ? (value as PublicTaskOutcomeState)
    : 'unknown';
}

export function normalizePublicTaskRollbackState(value: string): PublicTaskRollbackState {
  return knownRollbackStates.has(value as PublicTaskRollbackState)
    ? (value as PublicTaskRollbackState)
    : 'unknown';
}

export function normalizePublicTaskFinalizationState(value: string): PublicTaskFinalizationState {
  return knownFinalizationStates.has(value as PublicTaskFinalizationState)
    ? (value as PublicTaskFinalizationState)
    : 'unknown';
}

export function normalizePublicTaskNextAction(value: string): PublicTaskNextAction {
  if (value === 'none' || value === 'wait' || value === 'cancel_when_safe' || value === 'retry') {
    return value;
  }
  if (value === 'resume' || value === 'resolve_recovery' || value === 'recreate_plan') {
    return 'recovery';
  }
  return 'unknown';
}

export function normalizePublicTaskRemainingEffect(value: string): PublicTaskRemainingEffect {
  return knownRemainingEffects.has(value as PublicTaskRemainingEffect)
    ? (value as PublicTaskRemainingEffect)
    : 'unknown';
}

export function toMcpCenterSafeError(error: unknown): McpCenterSafeError {
  const envelope = readErrorEnvelope(error);
  const message = readErrorMessage(error);
  const fallbackCode = isConnectionErrorMessage(message) ? 'connection_unavailable' : 'unknown';
  const normalizedCode = normalizeSafeErrorCode(readString(envelope?.code));
  const code = normalizedCode === 'unknown' ? fallbackCode : normalizedCode;
  const retryable = readBoolean(envelope?.retryable) ?? fallbackCode === 'connection_unavailable';
  const correlationId = readString(envelope?.correlationId);
  const supportRef = createSupportRef(code, correlationId);

  return {
    code,
    retryable,
    supportRef,
    userCopyKey: toUserCopyKey(code),
  };
}

export function toPublicTask(task: McpTaskRef, referenceHint?: PublicTaskReference): PublicTask {
  const reference = rememberTaskReference(task.taskId, task.revision, referenceHint);
  const nextAction = normalizePublicTaskNextAction(task.outcome.nextAction);
  const status = normalizePublicTaskStatus(task.status);
  const displayState = taskStatusToDisplayState(status, nextAction);
  const error = task.outcome.error ? toMcpCenterSafeError(task.outcome.error) : undefined;

  return {
    reference,
    operation: normalizePublicTaskOperation(task.operation),
    status,
    displayState,
    progressPercent: clampProgress(task.progress),
    revision: task.revision,
    updatedAtMs: task.updatedAtMs,
    capabilities: createTaskCapabilities(status, nextAction, task.cancellable),
    outcome: {
      state: normalizePublicTaskOutcomeState(task.outcome.state),
      rollback: normalizePublicTaskRollbackState(task.outcome.rollback),
      finalization: normalizePublicTaskFinalizationState(task.outcome.finalization),
      remainingEffects: task.outcome.remainingEffects.map((effect) =>
        normalizePublicTaskRemainingEffect(effect)
      ),
      nextAction,
      ...(error ? { error } : {}),
    },
  };
}

export function toPublicTaskEvent(
  event: RawTaskEvent,
  taskReference?: PublicTaskReference
): PublicTaskEvent {
  const providedTaskEntry = taskReference
    ? getTaskReferenceEntry(taskReference.opaqueId, { touch: true })
    : null;
  const resolvedTaskReference =
    taskReference && providedTaskEntry?.taskId === event.taskId
      ? taskReference
      : (findMostRecentlyTouchedTaskReference(event.taskId) ?? rememberTaskReference(event.taskId));
  const reference = rememberEventReference(resolvedTaskReference, event.eventId);
  const payload = isRecord(event.payload) ? event.payload : null;
  const payloadType = readString(payload?.type);

  if (payloadType === 'task_created') {
    const status = normalizePublicTaskStatus(readString(payload?.status) ?? '');
    return {
      reference,
      taskReference: resolvedTaskReference,
      occurredAtMs: event.occurredAtMs,
      kind: 'created',
      displayState: taskStatusToEventDisplayState(status),
      status,
    };
  }

  if (payloadType === 'task_status_changed' || payloadType === 'confirmation_recorded') {
    const previousStatus = normalizePublicTaskStatus(readString(payload?.from) ?? '');
    const nextStatus = normalizePublicTaskStatus(readString(payload?.to) ?? '');
    const kind: PublicTaskEventKind =
      payloadType === 'confirmation_recorded' ? 'confirmation' : 'status_changed';
    return {
      reference,
      taskReference: resolvedTaskReference,
      occurredAtMs: event.occurredAtMs,
      kind,
      displayState: taskStatusToEventDisplayState(nextStatus),
      previousStatus,
      nextStatus,
      status: nextStatus,
    };
  }

  if (payloadType === 'cancellation_requested') {
    const previousStatus = normalizePublicTaskStatus(readString(payload?.from) ?? '');
    const nextStatus = normalizePublicTaskStatus(readString(payload?.to) ?? '');
    return {
      reference,
      taskReference: resolvedTaskReference,
      occurredAtMs: event.occurredAtMs,
      kind: 'cancellation_requested',
      displayState: taskStatusToEventDisplayState(nextStatus),
      previousStatus,
      nextStatus,
      status: nextStatus,
    };
  }

  if (payloadType === 'step_status_changed') {
    const checkpointState = normalizeStepState(readString(payload?.to));
    return {
      reference,
      taskReference: resolvedTaskReference,
      occurredAtMs: event.occurredAtMs,
      kind: 'step_status_changed',
      displayState: stepStateToDisplayState(checkpointState),
      checkpointOrdinal: readNumber(payload?.ordinal) ?? undefined,
      checkpointState,
    };
  }

  if (payloadType === 'recovery_decision') {
    const decision = isRecord(payload?.decision) ? payload.decision : null;
    const recoveryDecision = normalizeRecoveryDecision(readString(decision?.type));
    return {
      reference,
      taskReference: resolvedTaskReference,
      occurredAtMs: event.occurredAtMs,
      kind: 'recovery_decision',
      displayState: recoveryDecision === 'unknown' ? 'unknown' : 'recovery',
      recoveryDecision,
      checkpointOrdinal: readNumber(decision?.ordinal) ?? undefined,
    };
  }

  return {
    reference,
    taskReference: resolvedTaskReference,
    occurredAtMs: event.occurredAtMs,
    kind: 'unknown',
    displayState: 'unknown',
  };
}

export function resolvePublicTaskReference(
  reference: PublicTaskReference
): { taskId: string } | null {
  const resolved = getTaskReferenceEntry(reference.opaqueId, { touch: true });
  return resolved ? { taskId: resolved.taskId } : null;
}

export function resolvePublicTaskEventReference(
  reference: PublicTaskEventReference
): { eventId: number; taskId: string } | null {
  const resolved = getEventReferenceEntry(reference.opaqueId, { touch: true });
  return resolved ? { eventId: resolved.eventId, taskId: resolved.taskId } : null;
}

export function resolvePublicTaskMutationTarget(
  target: PublicTaskMutationTarget
): RawTaskResolution | null {
  const resolved = getTaskReferenceEntry(target.reference.opaqueId, { touch: true });
  return resolved ? { taskId: resolved.taskId, revision: target.revision } : null;
}

export function createMcpTaskMonitorCursor(
  task: Pick<McpTaskRef, 'revision' | 'taskId'>,
  referenceHint?: PublicTaskReference
): McpTaskMonitorCursor {
  const taskReference = rememberTaskReference(task.taskId, task.revision, referenceHint);
  return createCursor({
    afterEventId: undefined,
    eventOrder: {},
    leasedEventOpaqueIds: [],
    latestRevision: task.revision,
    rawTaskId: task.taskId,
    taskReference,
  });
}

export function revokePublicTaskReference(reference: PublicTaskReference) {
  deleteTaskReferenceEntry(reference.opaqueId);
}

export function releaseMcpTaskMonitorCursor(cursor: McpTaskMonitorCursor) {
  const state = readCursor(cursor);
  if (!state) return;
  taskMonitorCursorState.delete(cursor);
  releaseCursorLeases(state);
  revokePublicTaskReference(state.taskReference);
  pruneReferenceStore();
}

export function getMcpTaskMonitorResumeRequest(
  cursor: McpTaskMonitorCursor
): { afterEventId?: number; taskIds: [string] } | null {
  const state = readCursor(cursor);
  return state ? { afterEventId: state.afterEventId, taskIds: [state.rawTaskId] } : null;
}

export function mergeMcpTaskMonitorPage(
  cursor: McpTaskMonitorCursor,
  currentTask: PublicTask,
  currentEvents: readonly PublicTaskEvent[],
  latestTask: McpTaskRef,
  page: McpEventsPage,
  maxEvents = 8
): McpTaskMonitorMergeResult {
  const state = readCursor(cursor);
  if (!state || state.rawTaskId !== latestTask.taskId) {
    return rebuildMonitorCursorState(cursor, latestTask, page, maxEvents, state?.taskReference);
  }

  if (currentTask.reference.opaqueId !== state.taskReference.opaqueId) {
    return rebuildMonitorCursorState(cursor, latestTask, page, maxEvents, state.taskReference);
  }

  if (!canReuseCursorState(state, currentEvents)) {
    return rebuildMonitorCursorState(cursor, latestTask, page, maxEvents, state.taskReference);
  }

  const newerTask = selectNewerTask(state.rawTaskId, state.latestRevision, [
    latestTask,
    ...page.tasks,
  ]);
  const nextTask = newerTask ? toPublicTask(newerTask, state.taskReference) : currentTask;
  const mergedEvents = mergeOrderedEvents(
    currentEvents,
    state.eventOrder,
    page.events,
    state.taskReference,
    state.rawTaskId,
    maxEvents
  );
  setCursorState(cursor, {
    afterEventId: Math.max(state.afterEventId ?? 0, page.nextEventId),
    eventOrder: mergedEvents.eventOrder,
    leasedEventOpaqueIds: mergedEvents.events.map((event) => event.reference.opaqueId),
    latestRevision: newerTask?.revision ?? state.latestRevision,
    rawTaskId: state.rawTaskId,
    taskReference: state.taskReference,
  });

  return {
    cursor,
    events: mergedEvents.events,
    task: nextTask,
    taskChanged: !!newerTask,
    terminal: terminalTaskStatuses.has(nextTask.status),
  };
}

export function __resetOpaqueReferenceStoreForTests() {
  taskReferenceByOpaqueId.clear();
  taskOpaqueIdsByRawTaskId.clear();
  eventReferenceByOpaqueId.clear();
  opaqueEventIdByScopedRawKey.clear();
  taskMonitorCursorState = new WeakMap<McpTaskMonitorCursor, InternalTaskMonitorCursor>();
  referenceStoreNow = () => Date.now();
  taskReferenceTtlMs = DEFAULT_TASK_REFERENCE_TTL_MS;
  eventReferenceTtlMs = DEFAULT_EVENT_REFERENCE_TTL_MS;
  maxTaskReferences = DEFAULT_MAX_TASK_REFERENCES;
  maxEventReferences = DEFAULT_MAX_EVENT_REFERENCES;
}

export function __setOpaqueReferenceStoreNowForTests(now: () => number) {
  referenceStoreNow = now;
}

export function __setOpaqueReferenceStoreLimitsForTests({
  eventTtlMs,
  maxEvents,
  maxTasks,
  taskTtlMs,
}: {
  eventTtlMs?: number;
  maxEvents?: number;
  maxTasks?: number;
  taskTtlMs?: number;
}) {
  if (typeof taskTtlMs === 'number') taskReferenceTtlMs = taskTtlMs;
  if (typeof eventTtlMs === 'number') eventReferenceTtlMs = eventTtlMs;
  if (typeof maxTasks === 'number') maxTaskReferences = maxTasks;
  if (typeof maxEvents === 'number') maxEventReferences = maxEvents;
  pruneReferenceStore();
}
