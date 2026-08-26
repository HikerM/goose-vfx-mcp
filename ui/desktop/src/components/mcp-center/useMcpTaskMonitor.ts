import { useEffect, useRef, useState } from 'react';
import type { McpEventsPage, McpTaskRef } from '@hikerm/lumina-sdk';
import { getMcpTask, resumeMcpEvents } from '../../acp/mcp-platform';

const terminalStatuses = new Set<McpTaskRef['status']>([
  'succeeded',
  'failed',
  'cancelled',
  'interrupted',
  'recovery_required',
]);

type McpTaskMonitorEvent = McpEventsPage['events'][number];

export type McpTaskMonitorState = {
  events: McpTaskMonitorEvent[];
  eventsLoaded: boolean;
  paused: boolean;
};

const maxTimelineEvents = 8;

export function useMcpTaskMonitor(
  task: McpTaskRef | null,
  onUpdate: (task: McpTaskRef) => void,
  onError: (cause: unknown) => void,
  onTerminal: (task: McpTaskRef) => void,
  retryKey: number
): McpTaskMonitorState {
  const onUpdateRef = useRef(onUpdate);
  const onErrorRef = useRef(onError);
  const onTerminalRef = useRef(onTerminal);
  const afterEventId = useRef<number | undefined>(undefined);
  const [state, setState] = useState<McpTaskMonitorState>({
    events: [],
    eventsLoaded: false,
    paused: false,
  });
  const taskId = task?.taskId;
  const taskStatus = task?.status;
  const taskRevision = task?.revision;

  useEffect(() => {
    onUpdateRef.current = onUpdate;
    onErrorRef.current = onError;
    onTerminalRef.current = onTerminal;
  }, [onError, onTerminal, onUpdate]);

  useEffect(() => {
    afterEventId.current = undefined;
    setState({ events: [], eventsLoaded: false, paused: false });
    if (!taskId || !taskStatus || taskRevision === undefined) {
      return;
    }

    let active = true;
    let paused = false;
    let timeout: number | undefined;
    let latestRevision = taskRevision;
    const shouldPoll = !terminalStatuses.has(taskStatus);

    const mergeEvents = (incoming: McpTaskMonitorEvent[]) => {
      setState((current) => {
        const byId = new Map<number, McpTaskMonitorEvent>();
        for (const event of current.events) byId.set(event.eventId, event);
        for (const event of incoming) byId.set(event.eventId, event);
        const merged = Array.from(byId.values())
          .sort((left, right) => left.eventId - right.eventId)
          .slice(-maxTimelineEvents);
        return { events: merged, eventsLoaded: true, paused: false };
      });
    };

    const refresh = async () => {
      try {
        const [latest, events] = await Promise.all([
          getMcpTask(taskId),
          resumeMcpEvents([taskId], afterEventId.current),
        ]);
        if (!active) return;
        afterEventId.current = Math.max(afterEventId.current ?? 0, events.nextEventId);
        mergeEvents(events.events);
        const eventTask = events.tasks.find((item) => item.taskId === taskId);
        const candidate = eventTask && eventTask.revision > latest.revision ? eventTask : latest;
        if (candidate.revision > latestRevision) {
          latestRevision = candidate.revision;
          onUpdateRef.current(candidate);
          if (terminalStatuses.has(candidate.status)) {
            onTerminalRef.current(candidate);
            return;
          }
        }
      } catch (cause) {
        if (active) {
          paused = true;
          onErrorRef.current(cause);
          setState((current) => ({
            events: current.events,
            eventsLoaded: true,
            paused: true,
          }));
        }
        return;
      }
      if (active && shouldPoll && !paused) timeout = window.setTimeout(() => void refresh(), 2000);
    };

    void refresh();
    return () => {
      active = false;
      if (timeout !== undefined) window.clearTimeout(timeout);
    };
  }, [retryKey, taskId, taskRevision, taskStatus]);

  return state;
}
