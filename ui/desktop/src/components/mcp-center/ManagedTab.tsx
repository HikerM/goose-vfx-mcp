import { useCallback, useEffect, useRef, useState } from 'react';
import type {
  McpHealthStatus,
  McpManagedDetail,
  McpManagedSummary,
  McpPlanIntent,
  McpPlanReview,
  McpTaskRef,
} from '@hikerm/lumina-sdk';
import { Activity, Play, RotateCcw, Square, Trash2, Wrench } from 'lucide-react';
import { Button } from '../ui/button';
import { Switch } from '../ui/switch';
import {
  cancelMcpTask,
  controlMcpRuntime,
  createMcpPlan,
  getManagedMcp,
  getMcpHealth,
  listManagedMcps,
  retryMcpTask,
  runMcpHealth,
  sanitizeMcpUserMessage,
  setMcpDefaultEnabled,
  toMcpRecoveryViewModel,
  type McpPlatformRecoveryViewModel,
} from '../../acp/mcp-platform';
import {
  CredentialAttentionPanel,
  CredentialStatusBadge,
  DefinitionList,
  formatMcpValue,
  RecoveryPanel,
  StatePanel,
  StatusBadge,
} from './McpCenterCommon';
import { PlanReviewDialog } from './PlanReviewDialog';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../ui/dialog';
import { useMcpTaskMonitor } from './useMcpTaskMonitor';
import { mcpCenterMessages as messages } from './messages';
import { useIntl } from '../../i18n';

function taskTone(status: McpTaskRef['status']): 'neutral' | 'success' | 'warning' | 'danger' {
  if (status === 'succeeded') return 'success';
  if (['failed', 'interrupted', 'recovery_required'].includes(status)) return 'danger';
  if (status === 'cancelled') return 'warning';
  return 'neutral';
}

type ManagedTask = {
  managedMcpId: string;
  task: McpTaskRef;
};

type TaskSnapshot = {
  generation?: number;
  managedMcpId: string;
  task: McpTaskRef;
};

const terminalTaskStatuses = new Set<McpTaskRef['status']>([
  'succeeded',
  'failed',
  'cancelled',
  'interrupted',
  'recovery_required',
]);

type SafeMcpTaskEvent = {
  eventId: number;
  occurredAtMs: number;
  payload: {
    type: string;
    [key: string]: unknown;
  };
};

type TaskTimelineEntry = {
  id: string;
  occurredAtMs: number;
  title: string;
  detail?: string;
  tone: 'neutral' | 'success' | 'warning' | 'danger' | 'info';
};

function formatTaskTimestamp(intl: ReturnType<typeof useIntl>, updatedAtMs: number): string {
  return `${intl.formatDate(updatedAtMs, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  })} ${intl.formatTime(updatedAtMs, {
    hour: 'numeric',
    minute: '2-digit',
  })}`;
}

function hasUnsupportedRecoveryAction(action: McpTaskRef['outcome']['nextAction']): boolean {
  return action === 'resume' || action === 'resolve_recovery' || action === 'recreate_plan';
}

function planIntentOperation(intent: McpPlanIntent): McpTaskRef['operation'] {
  if (intent.type === 'register_catalog') {
    return 'register';
  }
  if (intent.type === 'install_catalog') {
    return 'install';
  }
  return intent.type;
}

function buildTaskRecoveryViewModel(
  intl: ReturnType<typeof useIntl>,
  selectedTask: McpTaskRef
): McpPlatformRecoveryViewModel | null {
  if (
    !selectedTask.outcome.error &&
    !['failed', 'interrupted', 'recovery_required'].includes(selectedTask.status)
  ) {
    return null;
  }
  return {
    title: intl.formatMessage(messages.taskRecoveryTitle, { status: selectedTask.status }),
    message:
      sanitizeMcpUserMessage(selectedTask.outcome.error?.message) ||
      intl.formatMessage(messages.taskErrorMessageByCode, {
        code: selectedTask.outcome.error?.code ?? 'unknown',
      }),
    nextStep: intl.formatMessage(messages.taskNextActionDescription, {
      action: selectedTask.outcome.nextAction,
    }),
    retryable: selectedTask.outcome.error?.retryable ?? false,
    correlationId: selectedTask.outcome.error?.correlationId,
    kind: 'service',
  };
}

function buildTaskTimelineEntries(
  intl: ReturnType<typeof useIntl>,
  events: SafeMcpTaskEvent[]
): TaskTimelineEntry[] {
  return [...events]
    .sort((left, right) => right.eventId - left.eventId)
    .map((event) => {
      const payload = event.payload;
      switch (payload.type) {
        case 'task_created':
          return {
            id: String(event.eventId),
            occurredAtMs: event.occurredAtMs,
            title: intl.formatMessage(messages.taskTimelineCreated),
            detail:
              typeof payload.status === 'string' ? formatMcpValue(intl, payload.status) : undefined,
            tone: 'info',
          };
        case 'task_status_changed':
          return {
            id: String(event.eventId),
            occurredAtMs: event.occurredAtMs,
            title: intl.formatMessage(messages.taskTimelineStatusChanged, {
              from:
                typeof payload.from === 'string' ? formatMcpValue(intl, payload.from) : 'Unknown',
              to: typeof payload.to === 'string' ? formatMcpValue(intl, payload.to) : 'Unknown',
            }),
            tone:
              payload.to === 'failed' || payload.to === 'interrupted'
                ? 'danger'
                : payload.to === 'recovery_required'
                  ? 'warning'
                  : payload.to === 'succeeded'
                    ? 'success'
                    : 'info',
          };
        case 'confirmation_recorded':
          return {
            id: String(event.eventId),
            occurredAtMs: event.occurredAtMs,
            title: intl.formatMessage(messages.taskTimelineConfirmation),
            detail: typeof payload.to === 'string' ? formatMcpValue(intl, payload.to) : undefined,
            tone: 'info',
          };
        case 'cancellation_requested':
          return {
            id: String(event.eventId),
            occurredAtMs: event.occurredAtMs,
            title: intl.formatMessage(messages.taskTimelineCancellation),
            detail: typeof payload.to === 'string' ? formatMcpValue(intl, payload.to) : undefined,
            tone: 'warning',
          };
        case 'step_status_changed':
          return {
            id: String(event.eventId),
            occurredAtMs: event.occurredAtMs,
            title: intl.formatMessage(messages.taskTimelineCheckpoint, {
              ordinal: typeof payload.ordinal === 'number' ? payload.ordinal : '?',
              status: typeof payload.to === 'string' ? payload.to : 'other',
            }),
            tone: payload.to === 'committed' ? 'success' : 'info',
          };
        case 'recovery_decision': {
          const decision = payload.decision as { type?: string; ordinal?: number } | undefined;
          return {
            id: String(event.eventId),
            occurredAtMs: event.occurredAtMs,
            title: intl.formatMessage(messages.taskTimelineRecoveryDecision, {
              decision: decision?.type ?? 'other',
              ordinal: decision?.ordinal ?? '?',
            }),
            tone: decision?.type === 'requires_manual_recovery' ? 'warning' : 'info',
          };
        }
        default:
          return {
            id: String(event.eventId),
            occurredAtMs: event.occurredAtMs,
            title: formatMcpValue(intl, payload.type),
            tone: 'neutral',
          };
      }
    });
}

type PendingManagedPlan = {
  plan: McpPlanReview;
  operation: McpTaskRef['operation'];
  managedMcpId: string;
  ownerRevision: number;
  selectionSequence: number;
};

export function ManagedTab({
  externalTask,
  onTaskChange,
}: {
  externalTask: McpTaskRef | null;
  onTaskChange: (task: McpTaskRef | null) => void;
}) {
  const intl = useIntl();
  const [items, setItems] = useState<McpManagedSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [nextCursor, setNextCursor] = useState<string>();
  const [error, setError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const [selected, setSelected] = useState<McpManagedSummary | null>(null);
  const [detail, setDetail] = useState<McpManagedDetail | null>(null);
  const [health, setHealth] = useState<McpHealthStatus | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  const [actionError, setActionError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const [pendingPlan, setPendingPlan] = useState<PendingManagedPlan | null>(null);
  const [stopConfirmation, setStopConfirmation] = useState<McpManagedSummary | null>(null);
  const [task, setTask] = useState<ManagedTask | null>(null);
  const [monitorRetryKey, setMonitorRetryKey] = useState(0);
  const selectionSequence = useRef(0);
  const selectedId = useRef<string | null>(null);
  const selectedRevision = useRef<number | null>(null);
  const taskRef = useRef<ManagedTask | null>(null);
  const taskGeneration = useRef(0);
  const listSequence = useRef(0);
  const actionSequence = useRef(0);
  const planSequence = useRef(0);
  const refreshSequences = useRef(new Map<string, number>());
  const externalResolutionTaskId = useRef<string | null>(null);

  const ownsSelection = useCallback(
    (managedMcpId: string, revision: number, sequence: number) =>
      selectedId.current === managedMcpId &&
      selectedRevision.current === revision &&
      selectionSequence.current === sequence,
    []
  );

  const updateTask = useCallback(
    (managedMcpId: string, next: McpTaskRef) => {
      if (selectedId.current !== managedMcpId) return;
      const current = taskRef.current;
      if (
        current?.task.taskId === next.taskId &&
        current.managedMcpId === managedMcpId &&
        current.task.revision >= next.revision
      ) {
        return;
      }
      const nextTask = { managedMcpId, task: next };
      taskGeneration.current += 1;
      taskRef.current = nextTask;
      setTask(nextTask);
      onTaskChange(next);
    },
    [onTaskChange]
  );

  const clearTask = useCallback(
    (managedMcpId: string) => {
      if (taskRef.current?.managedMcpId !== managedMcpId) return;
      taskGeneration.current += 1;
      taskRef.current = null;
      setTask(null);
      onTaskChange(null);
    },
    [onTaskChange]
  );

  const taskSnapshot = useCallback((managedMcpId: string, next: McpTaskRef): TaskSnapshot => {
    const current = taskRef.current;
    return {
      generation:
        current?.managedMcpId === managedMcpId &&
        current.task.taskId === next.taskId &&
        current.task.revision === next.revision
          ? taskGeneration.current
          : undefined,
      managedMcpId,
      task: next,
    };
  }, []);

  const ownsTaskSnapshot = useCallback((snapshot: TaskSnapshot) => {
    if (snapshot.generation === undefined) return true;
    const current = taskRef.current;
    return (
      taskGeneration.current === snapshot.generation &&
      current?.managedMcpId === snapshot.managedMcpId &&
      current.task.taskId === snapshot.task.taskId &&
      current.task.revision === snapshot.task.revision
    );
  }, []);

  const load = useCallback(async (cursor?: string) => {
    const sequence = ++listSequence.current;
    if (cursor) setLoadingMore(true);
    else setLoading(true);
    setError(null);
    try {
      const page = await listManagedMcps(cursor);
      if (listSequence.current !== sequence) return;
      setItems((current) => (cursor ? [...current, ...page.items] : page.items));
      setNextCursor(page.nextCursor ?? undefined);
    } catch (cause) {
      if (listSequence.current !== sequence) return;
      setError(toMcpRecoveryViewModel(cause));
    } finally {
      if (listSequence.current === sequence) {
        setLoading(false);
        setLoadingMore(false);
      }
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (!externalTask) return;
    const owner = items.find((item) => item.currentTask?.taskId === externalTask.taskId);
    if (owner) {
      externalResolutionTaskId.current = null;
      if (selectedId.current === owner.managedMcpId) {
        updateTask(owner.managedMcpId, externalTask);
      }
      return;
    }
    if (externalResolutionTaskId.current !== externalTask.taskId) {
      externalResolutionTaskId.current = externalTask.taskId;
      void load();
    }
  }, [externalTask, items, load, updateTask]);

  const selectItem = async (item: McpManagedSummary) => {
    const sequence = ++selectionSequence.current;
    actionSequence.current += 1;
    selectedId.current = item.managedMcpId;
    selectedRevision.current = item.revision;
    setSelected(item);
    setDetail(null);
    setHealth(null);
    setActionError(null);
    if (taskRef.current) {
      taskGeneration.current += 1;
      taskRef.current = null;
    }
    setTask(null);
    onTaskChange(null);
    setActionLoading(null);
    setDetailLoading(true);
    try {
      const [nextDetail, nextHealth] = await Promise.all([
        getManagedMcp(item.managedMcpId),
        getMcpHealth(item.managedMcpId),
      ]);
      if (
        selectionSequence.current !== sequence ||
        selectedId.current !== item.managedMcpId ||
        nextDetail.summary.managedMcpId !== item.managedMcpId ||
        nextHealth.managedMcpId !== item.managedMcpId
      ) {
        return;
      }
      selectedRevision.current = nextDetail.summary.revision;
      setSelected(nextDetail.summary);
      setItems((current) =>
        current.map((currentItem) =>
          currentItem.managedMcpId === nextDetail.summary.managedMcpId
            ? nextDetail.summary
            : currentItem
        )
      );
      setDetail(nextDetail);
      setHealth(nextHealth);
      if (nextDetail.summary.currentTask) {
        updateTask(nextDetail.summary.managedMcpId, nextDetail.summary.currentTask);
      } else {
        clearTask(nextDetail.summary.managedMcpId);
      }
    } catch (cause) {
      if (selectionSequence.current !== sequence) return;
      setActionError(toMcpRecoveryViewModel(cause));
    } finally {
      if (selectionSequence.current === sequence) setDetailLoading(false);
    }
  };

  const updateSummary = (next: McpManagedSummary) => {
    if (selectedId.current === next.managedMcpId) selectedRevision.current = next.revision;
    setSelected((current) => (current?.managedMcpId === next.managedMcpId ? next : current));
    setItems((current) =>
      current.map((item) => (item.managedMcpId === next.managedMcpId ? next : item))
    );
    setDetail((current) =>
      current?.summary.managedMcpId === next.managedMcpId ? { ...current, summary: next } : current
    );
  };

  const refreshManagedMcp = useCallback(
    async (
      managedMcpId: string,
      ownerRevision: number,
      ownerSelectionSequence: number,
      ownerTask: TaskSnapshot
    ) => {
      const refreshSequence = (refreshSequences.current.get(managedMcpId) ?? 0) + 1;
      refreshSequences.current.set(managedMcpId, refreshSequence);
      try {
        const [nextDetail, nextHealth] = await Promise.all([
          getManagedMcp(managedMcpId),
          getMcpHealth(managedMcpId),
        ]);
        if (
          nextDetail.summary.managedMcpId !== managedMcpId ||
          nextHealth.managedMcpId !== managedMcpId ||
          refreshSequences.current.get(managedMcpId) !== refreshSequence ||
          !ownsTaskSnapshot(ownerTask)
        ) {
          return;
        }
        const currentTask =
          ownerTask.generation === undefined ? ownerTask.task : (taskRef.current?.task ?? null);
        const refreshedTask = nextDetail.summary.currentTask;
        const canApplyRefreshedTask = refreshedTask
          ? refreshedTask.taskId === ownerTask.task.taskId &&
            refreshedTask.revision >= ownerTask.task.revision
          : !!currentTask && terminalTaskStatuses.has(currentTask.status);
        const nextSummary = canApplyRefreshedTask
          ? nextDetail.summary
          : { ...nextDetail.summary, currentTask };
        const reconciledDetail = { ...nextDetail, summary: nextSummary };
        setItems((current) => {
          return current.map((item) =>
            item.managedMcpId === managedMcpId &&
            item.revision === ownerRevision &&
            nextSummary.revision >= item.revision
              ? nextSummary
              : item
          );
        });
        if (ownsSelection(managedMcpId, ownerRevision, ownerSelectionSequence)) {
          selectedRevision.current = nextSummary.revision;
          setSelected(nextSummary);
          setDetail(reconciledDetail);
          setHealth(nextHealth);
          if (canApplyRefreshedTask) {
            if (refreshedTask) {
              updateTask(managedMcpId, refreshedTask);
            } else {
              clearTask(managedMcpId);
            }
          }
        }
      } catch (cause) {
        if (
          ownsSelection(managedMcpId, ownerRevision, ownerSelectionSequence) &&
          refreshSequences.current.get(managedMcpId) === refreshSequence &&
          ownsTaskSnapshot(ownerTask)
        ) {
          setActionError(toMcpRecoveryViewModel(cause));
        }
      }
    },
    [clearTask, ownsSelection, ownsTaskSnapshot, updateTask]
  );

  const handleMonitorUpdate = useCallback(
    (next: McpTaskRef) => {
      const current = taskRef.current;
      if (
        !task ||
        selectedId.current !== task.managedMcpId ||
        next.taskId !== task.task.taskId ||
        current?.managedMcpId !== task.managedMcpId ||
        current.task.taskId !== task.task.taskId
      )
        return;
      updateTask(task.managedMcpId, next);
      setActionError(null);
    },
    [task, updateTask]
  );
  const handleMonitorError = useCallback(
    (cause: unknown) => {
      if (!task || selectedId.current !== task.managedMcpId) return;
      setActionError({ ...toMcpRecoveryViewModel(cause), kind: 'monitor', retryable: true });
    },
    [task]
  );
  const handleTerminalTask = useCallback(
    (next: McpTaskRef) => {
      const current = taskRef.current;
      if (
        !task ||
        selectedId.current !== task.managedMcpId ||
        next.taskId !== task.task.taskId ||
        current?.managedMcpId !== task.managedMcpId ||
        current.task.taskId !== next.taskId ||
        current.task.revision !== next.revision
      )
        return;
      const ownerRevision = selectedRevision.current;
      if (ownerRevision === null) return;
      void refreshManagedMcp(
        task.managedMcpId,
        ownerRevision,
        selectionSequence.current,
        taskSnapshot(task.managedMcpId, next)
      );
    },
    [refreshManagedMcp, task, taskSnapshot]
  );
  const taskMonitor = useMcpTaskMonitor(
    task?.task ?? null,
    handleMonitorUpdate,
    handleMonitorError,
    handleTerminalTask,
    monitorRetryKey
  );

  const toggleEnabled = async (enabled: boolean) => {
    if (!selected) return;
    const owner = selected.managedMcpId;
    const ownerRevision = selected.revision;
    const ownerSelectionSequence = selectionSequence.current;
    const action = ++actionSequence.current;
    setActionLoading('toggle');
    setActionError(null);
    try {
      const next = await setMcpDefaultEnabled(selected, enabled);
      if (
        actionSequence.current !== action ||
        !ownsSelection(owner, ownerRevision, ownerSelectionSequence) ||
        next.managedMcpId !== owner
      ) {
        return;
      }
      updateSummary(next);
    } catch (cause) {
      if (
        actionSequence.current !== action ||
        !ownsSelection(owner, ownerRevision, ownerSelectionSequence)
      )
        return;
      setActionError(toMcpRecoveryViewModel(cause));
    } finally {
      if (actionSequence.current === action) setActionLoading(null);
    }
  };

  const runHealth = async () => {
    if (!selected) return;
    const owner = selected.managedMcpId;
    const ownerRevision = selected.revision;
    const ownerSelectionSequence = selectionSequence.current;
    const action = ++actionSequence.current;
    setActionLoading('health');
    setActionError(null);
    try {
      const nextTask = await runMcpHealth(
        owner,
        selected.installation === 'not_applicable' ? 'registration' : 'runtime'
      );
      if (
        actionSequence.current !== action ||
        !ownsSelection(owner, ownerRevision, ownerSelectionSequence)
      )
        return;
      updateTask(owner, nextTask);
      const nextHealth = await getMcpHealth(owner);
      if (
        actionSequence.current !== action ||
        !ownsSelection(owner, ownerRevision, ownerSelectionSequence) ||
        nextHealth.managedMcpId !== owner
      ) {
        return;
      }
      setHealth(nextHealth);
    } catch (cause) {
      if (
        actionSequence.current !== action ||
        !ownsSelection(owner, ownerRevision, ownerSelectionSequence)
      )
        return;
      setActionError(toMcpRecoveryViewModel(cause));
    } finally {
      if (actionSequence.current === action) setActionLoading(null);
    }
  };

  const controlRuntime = async (action: 'start' | 'stop') => {
    if (!selected) return;
    const owner = selected.managedMcpId;
    const ownerRevision = selected.revision;
    const ownerSelectionSequence = selectionSequence.current;
    const actionRequest = ++actionSequence.current;
    setActionLoading(`runtime-${action}`);
    setActionError(null);
    try {
      const next = await controlMcpRuntime(selected, action);
      if (
        actionSequence.current !== actionRequest ||
        !ownsSelection(owner, ownerRevision, ownerSelectionSequence) ||
        next.managedMcpId !== owner
      )
        return;
      updateSummary(next);
    } catch (cause) {
      if (
        actionSequence.current === actionRequest &&
        ownsSelection(owner, ownerRevision, ownerSelectionSequence)
      ) {
        setActionError(toMcpRecoveryViewModel(cause));
      }
    } finally {
      if (actionSequence.current === actionRequest) setActionLoading(null);
    }
  };

  const openPlan = async (intent: McpPlanIntent, action: string) => {
    if (!selected) return;
    const owner = selected.managedMcpId;
    const ownerRevision = selected.revision;
    const ownerSelectionSequence = selectionSequence.current;
    const actionRequest = ++actionSequence.current;
    const planRequest = ++planSequence.current;
    setActionLoading(action);
    setActionError(null);
    try {
      const nextPlan = await createMcpPlan(intent);
      if (planSequence.current !== planRequest) return;
      setPendingPlan({
        plan: nextPlan,
        operation: planIntentOperation(intent),
        managedMcpId: owner,
        ownerRevision,
        selectionSequence: ownerSelectionSequence,
      });
    } catch (cause) {
      if (
        actionSequence.current !== actionRequest ||
        !ownsSelection(owner, ownerRevision, ownerSelectionSequence)
      )
        return;
      setActionError(toMcpRecoveryViewModel(cause));
    } finally {
      if (actionSequence.current === actionRequest) setActionLoading(null);
    }
  };

  const cancelTask = async () => {
    if (!task || task.managedMcpId !== selected?.managedMcpId) return;
    const ownerTask = task;
    const ownerRevision = selected.revision;
    const ownerSelectionSequence = selectionSequence.current;
    const action = ++actionSequence.current;
    setActionLoading('cancel-task');
    try {
      const next = await cancelMcpTask(ownerTask.task);
      const currentTask = taskRef.current;
      if (
        actionSequence.current !== action ||
        !ownsSelection(ownerTask.managedMcpId, ownerRevision, ownerSelectionSequence) ||
        currentTask?.managedMcpId !== ownerTask.managedMcpId ||
        currentTask.task.taskId !== ownerTask.task.taskId ||
        currentTask.task.revision !== ownerTask.task.revision
      )
        return;
      updateTask(ownerTask.managedMcpId, next);
    } catch (cause) {
      const currentTask = taskRef.current;
      if (
        actionSequence.current !== action ||
        !ownsSelection(ownerTask.managedMcpId, ownerRevision, ownerSelectionSequence) ||
        currentTask?.managedMcpId !== ownerTask.managedMcpId ||
        currentTask.task.taskId !== ownerTask.task.taskId ||
        currentTask.task.revision !== ownerTask.task.revision
      )
        return;
      setActionError(toMcpRecoveryViewModel(cause));
    } finally {
      if (actionSequence.current === action) setActionLoading(null);
    }
  };

  const retryTask = async () => {
    if (!task || task.managedMcpId !== selected?.managedMcpId) return;
    const ownerTask = task;
    const ownerRevision = selected.revision;
    const ownerSelectionSequence = selectionSequence.current;
    const action = ++actionSequence.current;
    setActionLoading('retry-task');
    try {
      const next = await retryMcpTask(ownerTask.task);
      const currentTask = taskRef.current;
      if (
        actionSequence.current !== action ||
        !ownsSelection(ownerTask.managedMcpId, ownerRevision, ownerSelectionSequence) ||
        currentTask?.managedMcpId !== ownerTask.managedMcpId ||
        currentTask.task.taskId !== ownerTask.task.taskId ||
        currentTask.task.revision !== ownerTask.task.revision
      )
        return;
      updateTask(ownerTask.managedMcpId, next);
    } catch (cause) {
      const currentTask = taskRef.current;
      if (
        actionSequence.current !== action ||
        !ownsSelection(ownerTask.managedMcpId, ownerRevision, ownerSelectionSequence) ||
        currentTask?.managedMcpId !== ownerTask.managedMcpId ||
        currentTask.task.taskId !== ownerTask.task.taskId ||
        currentTask.task.revision !== ownerTask.task.revision
      )
        return;
      setActionError(toMcpRecoveryViewModel(cause));
    } finally {
      if (actionSequence.current === action) setActionLoading(null);
    }
  };

  const selectedTask = task && task.managedMcpId === selected?.managedMcpId ? task.task : null;
  const selectedTaskRecovery = selectedTask ? buildTaskRecoveryViewModel(intl, selectedTask) : null;
  const selectedTaskTimeline = selectedTask
    ? buildTaskTimelineEntries(intl, taskMonitor.events as SafeMcpTaskEvent[])
    : [];
  const canRetryTask =
    !!selectedTask?.outcome.error?.retryable && selectedTask.outcome.nextAction === 'retry';
  const taskActionNotes = selectedTask
    ? [
        selectedTask.cancellable ? intl.formatMessage(messages.taskCancelImpact) : null,
        canRetryTask ? intl.formatMessage(messages.taskRetryImpact) : null,
        hasUnsupportedRecoveryAction(selectedTask.outcome.nextAction)
          ? intl.formatMessage(messages.taskUnsupportedRecoveryAction, {
              action: formatMcpValue(intl, selectedTask.outcome.nextAction),
            })
          : null,
      ].filter((note): note is string => !!note)
    : [];
  const taskTimelineFallbackMessage = selectedTask
    ? !taskMonitor.eventsLoaded
      ? intl.formatMessage(messages.taskTimelineWaiting)
      : selectedTaskTimeline.length === 0
        ? intl.formatMessage(
            terminalTaskStatuses.has(selectedTask.status)
              ? messages.taskTimelineUnavailableDone
              : messages.taskTimelineUnavailableActive
          )
        : null
    : null;
  const selectedAvailableVersion = selected?.availableVersion ?? null;
  const selectedCredentialStatus =
    health?.credentialStatus ?? detail?.summary.credentialStatus ?? selected?.credentialStatus;

  return (
    <div className="grid min-h-0 gap-5 xl:grid-cols-[minmax(300px,0.42fr)_minmax(0,1fr)]">
      <section aria-label={intl.formatMessage(messages.managedInventory)} className="space-y-3">
        {loading ? (
          <StatePanel
            kind="loading"
            title={intl.formatMessage(messages.loadingManaged)}
            description={intl.formatMessage(messages.readingManaged)}
          />
        ) : error ? (
          <RecoveryPanel recovery={error} onRetry={() => void load()} />
        ) : items.length === 0 ? (
          <StatePanel
            kind="empty"
            title={intl.formatMessage(messages.noManaged)}
            description={intl.formatMessage(messages.noManagedDescription)}
          />
        ) : (
          <>
            {items.map((item) => (
              <button
                key={item.managedMcpId}
                onClick={() => void selectItem(item)}
                className="w-full rounded-xl border border-border-primary bg-background-primary p-4 text-left hover:bg-background-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring-info"
                aria-pressed={selected?.managedMcpId === item.managedMcpId}
              >
                <div className="flex items-center justify-between gap-3">
                  <h3 className="truncate font-medium text-text-primary">{item.mcpId}</h3>
                  <StatusBadge
                    tone={
                      item.health === 'healthy'
                        ? 'success'
                        : item.health === 'unknown'
                          ? 'neutral'
                          : 'warning'
                    }
                  >
                    {formatMcpValue(intl, item.health)}
                  </StatusBadge>
                </div>
                <div className="mt-3 flex flex-wrap gap-1.5">
                  <StatusBadge>{formatMcpValue(intl, item.registration)}</StatusBadge>
                  <StatusBadge>{formatMcpValue(intl, item.installation)}</StatusBadge>
                  <StatusBadge>{formatMcpValue(intl, item.runtime)}</StatusBadge>
                  <CredentialStatusBadge status={item.credentialStatus} />
                  {item.recoveryRequired && (
                    <StatusBadge tone="danger">
                      {intl.formatMessage(messages.recoveryRequired)}
                    </StatusBadge>
                  )}
                </div>
                <p className="mt-3 text-xs text-text-secondary">
                  {item.activeVersion ?? intl.formatMessage(messages.noActiveVersion)} ·{' '}
                  {formatMcpValue(intl, item.nextAction)}
                </p>
              </button>
            ))}
            {nextCursor && (
              <Button
                className="w-full"
                variant="outline"
                disabled={loadingMore}
                onClick={() => void load(nextCursor)}
              >
                {intl.formatMessage(loadingMore ? messages.loading : messages.loadMore)}
              </Button>
            )}
          </>
        )}
      </section>

      <section aria-label={intl.formatMessage(messages.managedDetails)} className="min-w-0">
        {!selected ? (
          <StatePanel
            kind="empty"
            title={intl.formatMessage(messages.selectManaged)}
            description={intl.formatMessage(messages.selectManagedDescription)}
          />
        ) : detailLoading ? (
          <StatePanel
            kind="loading"
            title={intl.formatMessage(messages.loadingMcpState)}
            description={intl.formatMessage(messages.readingMcpState)}
          />
        ) : (
          <div className="space-y-5 rounded-xl border border-border-primary bg-background-primary p-5">
            {actionError && (
              <RecoveryPanel
                recovery={actionError}
                onRetry={
                  actionError.kind === 'monitor'
                    ? () => {
                        setActionError(null);
                        setMonitorRetryKey((current) => current + 1);
                      }
                    : undefined
                }
              />
            )}
            <div className="flex flex-wrap items-start justify-between gap-4">
              <div>
                <h2 className="text-xl font-medium text-text-primary">{selected.mcpId}</h2>
                <p
                  className="mt-1 truncate text-sm text-text-secondary"
                  title={selected.managedMcpId}
                  aria-label={selected.managedMcpId}
                >
                  {selected.managedMcpId}
                </p>
              </div>
              <label className="flex items-center gap-2 text-sm text-text-primary">
                {intl.formatMessage(messages.defaultEnabled)}
                <Switch
                  variant="mono"
                  checked={selected.defaultEnabled}
                  disabled={actionLoading === 'toggle'}
                  onCheckedChange={(checked) => void toggleEnabled(checked)}
                  aria-label={intl.formatMessage(messages.defaultEnabled)}
                />
              </label>
            </div>

            <DefinitionList
              items={[
                [
                  intl.formatMessage(messages.registration),
                  formatMcpValue(intl, selected.registration),
                ],
                [
                  intl.formatMessage(messages.installation),
                  formatMcpValue(intl, selected.installation),
                ],
                [intl.formatMessage(messages.runtime), formatMcpValue(intl, selected.runtime)],
                [
                  intl.formatMessage(messages.health),
                  formatMcpValue(intl, health?.state ?? selected.health),
                ],
                [
                  intl.formatMessage(messages.credentialStatusLabel),
                  <CredentialStatusBadge
                    key="managed-credential-status"
                    status={selectedCredentialStatus}
                  />,
                ],
                [
                  intl.formatMessage(messages.activeVersion),
                  selected.activeVersion ?? intl.formatMessage(messages.noValue),
                ],
                [
                  intl.formatMessage(messages.availableVersion),
                  selected.availableVersion ?? intl.formatMessage(messages.noValue),
                ],
                [intl.formatMessage(messages.adapter), selected.distributionAdapter],
                [
                  intl.formatMessage(messages.nextAction),
                  formatMcpValue(intl, selected.nextAction),
                ],
              ]}
            />

            <CredentialAttentionPanel
              status={selectedCredentialStatus}
              actions={[
                {
                  label: intl.formatMessage(messages.credentialRefresh),
                  disabled: !!actionLoading,
                  onClick: () => void selectItem(selected),
                },
              ]}
            />

            {health?.latest && (
              <div className="rounded-lg bg-background-secondary p-3 text-sm text-text-secondary">
                {intl.formatMessage(messages.lastHealthCheck, {
                  result: formatMcpValue(intl, health.latest.result),
                  latency: health.latest.latencyMs,
                  detail: formatMcpValue(intl, health.latest.detailCode),
                })}
              </div>
            )}

            <div className="flex flex-wrap gap-2">
              <div
                aria-busy={actionLoading === 'runtime-start' || actionLoading === 'runtime-stop'}
                aria-live="polite"
                className="sr-only"
                role="status"
              >
                {actionLoading === 'runtime-start' && intl.formatMessage(messages.startingRuntime)}
                {actionLoading === 'runtime-stop' && intl.formatMessage(messages.stoppingRuntime)}
              </div>
              {selected.runtimeControl?.canStart && (
                <Button
                  variant="outline"
                  disabled={!!actionLoading}
                  aria-busy={actionLoading === 'runtime-start'}
                  onClick={() => void controlRuntime('start')}
                >
                  <Play />{' '}
                  {intl.formatMessage(
                    actionLoading === 'runtime-start'
                      ? messages.startingRuntime
                      : messages.startRuntime
                  )}
                </Button>
              )}
              {selected.runtimeControl?.canStop && (
                <Button
                  variant="outline"
                  disabled={!!actionLoading}
                  aria-busy={actionLoading === 'runtime-stop'}
                  onClick={() => setStopConfirmation(selected)}
                >
                  <Square />{' '}
                  {intl.formatMessage(
                    actionLoading === 'runtime-stop'
                      ? messages.stoppingRuntime
                      : messages.stopRuntime
                  )}
                </Button>
              )}
              <Button variant="outline" disabled={!!actionLoading} onClick={() => void runHealth()}>
                <Activity />{' '}
                {intl.formatMessage(
                  actionLoading === 'health' ? messages.checking : messages.runHealth
                )}
              </Button>
              {selected.eligibility.update && selectedAvailableVersion && (
                <Button
                  variant="outline"
                  disabled={!!actionLoading}
                  onClick={() =>
                    void openPlan(
                      {
                        type: 'update',
                        managed_mcp_id: selected.managedMcpId,
                        target_version: selectedAvailableVersion,
                      },
                      'update'
                    )
                  }
                >
                  <RotateCcw /> {intl.formatMessage(messages.reviewUpdate)}
                </Button>
              )}
              {selected.eligibility.repair && (
                <Button
                  variant="outline"
                  disabled={!!actionLoading}
                  onClick={() =>
                    void openPlan(
                      { type: 'repair', managed_mcp_id: selected.managedMcpId },
                      'repair'
                    )
                  }
                >
                  <Wrench /> {intl.formatMessage(messages.reviewRepair)}
                </Button>
              )}
              {selected.eligibility.uninstall && (
                <Button
                  variant="destructive"
                  disabled={!!actionLoading}
                  onClick={() =>
                    void openPlan(
                      {
                        type: 'uninstall',
                        managed_mcp_id: selected.managedMcpId,
                        preserve_user_data: true,
                      },
                      'uninstall'
                    )
                  }
                >
                  <Trash2 /> {intl.formatMessage(messages.reviewUninstall)}
                </Button>
              )}
            </div>

            {selectedTask && (
              <section className="rounded-xl border border-border-primary p-4" aria-live="polite">
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div>
                    <h3 className="font-medium text-text-primary">
                      {intl.formatMessage(messages.currentTask)}
                    </h3>
                    <p className="mt-1 text-sm text-text-secondary">
                      {intl.formatMessage(messages.taskStatusTitle, {
                        status: selectedTask.status,
                        operation: formatMcpValue(intl, selectedTask.operation),
                      })}
                    </p>
                  </div>
                  <StatusBadge tone={taskTone(selectedTask.status)}>
                    {formatMcpValue(intl, selectedTask.status)}
                  </StatusBadge>
                </div>
                <p className="mt-3 text-sm text-text-secondary">
                  {intl.formatMessage(messages.taskStatusDescription, {
                    status: selectedTask.status,
                  })}
                </p>
                <div className="mt-3 h-2 overflow-hidden rounded-full bg-background-secondary">
                  <div
                    className="h-full bg-background-info transition-all"
                    style={{ width: `${selectedTask.progress}%` }}
                  />
                </div>
                <p className="mt-2 text-xs text-text-tertiary">
                  {formatMcpValue(intl, selectedTask.operation)} · {selectedTask.progress}% ·{' '}
                  {intl.formatMessage(messages.latestUpdate)}:{' '}
                  {formatTaskTimestamp(intl, selectedTask.updatedAtMs)}
                </p>

                <div className="mt-4 rounded-lg bg-background-secondary/40 p-3">
                  <DefinitionList
                    items={[
                      [
                        intl.formatMessage(messages.currentPhase),
                        formatMcpValue(intl, selectedTask.status),
                      ],
                      [
                        intl.formatMessage(messages.outcomeState),
                        formatMcpValue(intl, selectedTask.outcome.state),
                      ],
                      [
                        intl.formatMessage(messages.finalizationState),
                        formatMcpValue(intl, selectedTask.outcome.finalization),
                      ],
                      [
                        intl.formatMessage(messages.rollbackState),
                        formatMcpValue(intl, selectedTask.outcome.rollback),
                      ],
                      [
                        intl.formatMessage(messages.recommendation),
                        intl.formatMessage(messages.taskNextActionDescription, {
                          action: selectedTask.outcome.nextAction,
                        }),
                      ],
                      [
                        intl.formatMessage(messages.remainingEffects),
                        selectedTask.outcome.remainingEffects.length === 0 ? (
                          intl.formatMessage(messages.taskNoRemainingEffects)
                        ) : (
                          <div className="flex flex-wrap gap-1.5">
                            {selectedTask.outcome.remainingEffects.map((effect) => (
                              <StatusBadge key={effect}>{formatMcpValue(intl, effect)}</StatusBadge>
                            ))}
                          </div>
                        ),
                      ],
                    ]}
                  />
                </div>

                {selectedTaskRecovery && (
                  <div className="mt-4">
                    <RecoveryPanel recovery={selectedTaskRecovery} />
                  </div>
                )}

                <div className="mt-4 rounded-lg border border-border-primary p-3">
                  <div className="flex items-center justify-between gap-3">
                    <h4 className="font-medium text-text-primary">
                      {intl.formatMessage(messages.recentActivity)}
                    </h4>
                    <span className="text-xs text-text-tertiary">
                      {taskMonitor.eventsLoaded
                        ? `${selectedTaskTimeline.length}`
                        : intl.formatMessage(messages.loading)}
                    </span>
                  </div>
                  {taskTimelineFallbackMessage ? (
                    <p className="mt-2 text-sm text-text-secondary">
                      {taskTimelineFallbackMessage}
                    </p>
                  ) : (
                    <ol className="mt-3 space-y-3">
                      {selectedTaskTimeline.map((entry) => (
                        <li key={entry.id} className="rounded-lg bg-background-secondary/50 p-3">
                          <div className="flex flex-wrap items-center justify-between gap-2">
                            <StatusBadge tone={entry.tone}>{entry.title}</StatusBadge>
                            <span className="text-xs text-text-tertiary">
                              {formatTaskTimestamp(intl, entry.occurredAtMs)}
                            </span>
                          </div>
                          {entry.detail && (
                            <p className="mt-2 text-sm text-text-secondary">{entry.detail}</p>
                          )}
                        </li>
                      ))}
                    </ol>
                  )}
                </div>

                <div className="mt-4 rounded-lg border border-border-primary p-3">
                  <h4 className="font-medium text-text-primary">
                    {intl.formatMessage(messages.taskActionContext)}
                  </h4>
                  <p className="mt-2 text-sm text-text-secondary">
                    {intl.formatMessage(messages.taskSafetyBoundary)}
                  </p>
                  {taskActionNotes.length > 0 && (
                    <div className="mt-3 space-y-2 text-sm text-text-secondary">
                      {taskActionNotes.map((note) => (
                        <p key={note}>{note}</p>
                      ))}
                    </div>
                  )}
                  <div className="mt-3 flex flex-wrap gap-2">
                    {selectedTask.cancellable && (
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={!!actionLoading}
                        onClick={() => void cancelTask()}
                      >
                        {intl.formatMessage(
                          actionLoading === 'cancel-task'
                            ? messages.cancelling
                            : messages.cancelTask
                        )}
                      </Button>
                    )}
                    {canRetryTask && (
                      <Button size="sm" disabled={!!actionLoading} onClick={() => void retryTask()}>
                        {intl.formatMessage(
                          actionLoading === 'retry-task' ? messages.retrying : messages.retryTask
                        )}
                      </Button>
                    )}
                  </div>
                </div>
              </section>
            )}

            {detail && (
              <p className="truncate text-xs text-text-tertiary">
                {intl.formatMessage(messages.activeManifest, {
                  value: detail.activeManifestDigest,
                })}
              </p>
            )}
          </div>
        )}
      </section>

      <PlanReviewDialog
        plan={pendingPlan?.plan ?? null}
        operation={pendingPlan?.operation ?? null}
        onClose={() => {
          setPendingPlan(null);
        }}
        onTaskCreated={(nextTask) => {
          if (!pendingPlan) return;
          const { managedMcpId, ownerRevision, selectionSequence: ownerSequence } = pendingPlan;
          if (ownsSelection(managedMcpId, ownerRevision, ownerSequence)) {
            updateTask(managedMcpId, nextTask);
          }
          void refreshManagedMcp(
            managedMcpId,
            ownerRevision,
            ownerSequence,
            taskSnapshot(managedMcpId, nextTask)
          );
        }}
      />
      <Dialog
        open={!!stopConfirmation}
        onOpenChange={(open) => !open && !actionLoading && setStopConfirmation(null)}
      >
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>{intl.formatMessage(messages.stopRuntimeTitle)}</DialogTitle>
            <DialogDescription>
              {intl.formatMessage(messages.stopRuntimeDescription)}
            </DialogDescription>
          </DialogHeader>
          <p className="text-sm text-text-secondary">
            {intl.formatMessage(messages.stopRuntimeImpact)}
          </p>
          <DialogFooter>
            <Button
              variant="outline"
              disabled={!!actionLoading}
              onClick={() => setStopConfirmation(null)}
            >
              {intl.formatMessage(messages.cancel)}
            </Button>
            <Button
              variant="destructive"
              disabled={!!actionLoading}
              aria-busy={actionLoading === 'runtime-stop'}
              onClick={() => {
                setStopConfirmation(null);
                void controlRuntime('stop');
              }}
            >
              <Square />{' '}
              {intl.formatMessage(
                actionLoading === 'runtime-stop'
                  ? messages.stoppingRuntime
                  : messages.stopRuntimeConfirm
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
