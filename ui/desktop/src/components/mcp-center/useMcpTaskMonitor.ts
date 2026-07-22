import { useEffect, useRef } from 'react';
import type { McpTaskRef } from '@aaif/goose-sdk';
import { getMcpTask, resumeMcpEvents } from '../../acp/mcp-platform';

const terminalStatuses = new Set<McpTaskRef['status']>([
  'succeeded',
  'failed',
  'cancelled',
  'interrupted',
  'recovery_required',
]);

export function useMcpTaskMonitor(
  task: McpTaskRef | null,
  onUpdate: (task: McpTaskRef) => void,
  onError: (cause: unknown) => void,
  onTerminal: (task: McpTaskRef) => void,
  retryKey: number
): void {
  const afterEventId = useRef<number | undefined>(undefined);
  const taskId = task?.taskId;
  const taskStatus = task?.status;
  const taskRevision = task?.revision;

  useEffect(() => {
    afterEventId.current = undefined;
    if (!taskId || !taskStatus || taskRevision === undefined || terminalStatuses.has(taskStatus)) {
      return;
    }

    let active = true;
    let timeout: number | undefined;
    let latestRevision = taskRevision;

    const refresh = async () => {
      try {
        const [latest, events] = await Promise.all([
          getMcpTask(taskId),
          resumeMcpEvents([taskId], afterEventId.current),
        ]);
        if (!active) return;
        afterEventId.current = events.nextEventId;
        const eventTask = events.tasks.find((item) => item.taskId === taskId);
        const candidate = eventTask && eventTask.revision > latest.revision ? eventTask : latest;
        if (candidate.revision > latestRevision) {
          latestRevision = candidate.revision;
          onUpdate(candidate);
          if (terminalStatuses.has(candidate.status)) {
            onTerminal(candidate);
            return;
          }
        }
      } catch (cause) {
        if (active) onError(cause);
      }
      if (active) timeout = window.setTimeout(() => void refresh(), 2000);
    };

    void refresh();
    return () => {
      active = false;
      if (timeout !== undefined) window.clearTimeout(timeout);
    };
  }, [onError, onTerminal, onUpdate, retryKey, taskId, taskRevision, taskStatus]);
}
