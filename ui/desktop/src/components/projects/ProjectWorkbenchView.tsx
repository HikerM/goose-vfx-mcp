import { useCallback, useEffect, useMemo, useState } from 'react';
import { ArrowLeft, FolderGit2, GitBranch, Loader2, Plus, RefreshCw } from 'lucide-react';
import { useNavigate, useParams, useSearchParams } from 'react-router-dom';
import { toast } from 'react-toastify';
import { acpProjectSnapshot, type ProjectSnapshot } from '../../acp/projects';
import { acpListProjectSessions, acpRenameSession, type SessionListItem } from '../../acp/sessions';
import { AppEvents } from '../../constants/events';
import { defineMessages, useIntl } from '../../i18n';
import { createSession } from '../../sessions';
import type { Session } from '../../types/session';
import { errorMessage } from '../../utils/conversionUtils';
import { sessionToListItem } from '../../hooks/useNavigationSessions';
import { InlineEditText } from '../common/InlineEditText';
import { useConfig } from '../ConfigContext';
import { SessionIndicators } from '../SessionIndicators';
import { Button } from '../ui/button';
import { cn } from '../../utils';
import { projectTaskPath } from '../../utils/navigationUtils';

type StreamState = 'idle' | 'loading' | 'streaming' | 'error';

interface SessionStatus {
  streamState: StreamState;
  hasUnreadActivity: boolean;
}

const i18n = defineMessages({
  back: { id: 'projectWorkspace.back', defaultMessage: 'Back to projects' },
  newTask: { id: 'projectWorkspace.newTask', defaultMessage: 'New task' },
  creating: { id: 'projectWorkspace.creating', defaultMessage: 'Creating…' },
  tasks: { id: 'projectWorkspace.tasks', defaultMessage: 'Tasks' },
  refresh: { id: 'projectWorkspace.refresh', defaultMessage: 'Refresh tasks' },
  empty: { id: 'projectWorkspace.empty', defaultMessage: 'No tasks in this project yet.' },
  untitled: { id: 'projectWorkspace.untitled', defaultMessage: 'Untitled task' },
  loading: { id: 'projectWorkspace.loading', defaultMessage: 'Loading project…' },
  unavailable: {
    id: 'projectWorkspace.unavailable',
    defaultMessage: 'Project is unavailable.',
  },
});

function sessionActivityAt(session: SessionListItem): string {
  return session.lastMessageAt ?? session.updatedAt ?? session.createdAt;
}

function ProjectSessionRow({
  session,
  active,
  status,
  onSelect,
  onRenamed,
}: {
  session: SessionListItem;
  active: boolean;
  status?: SessionStatus;
  onSelect: () => void;
  onRenamed: (name: string) => void;
}) {
  const intl = useIntl();
  const [editing, setEditing] = useState(false);
  const activityAt = sessionActivityAt(session);
  const activityLabel = activityAt ? new Date(activityAt).toLocaleDateString() : '';
  const isStreaming = status?.streamState === 'streaming';

  return (
    <div
      role="button"
      tabIndex={0}
      aria-current={active ? 'page' : undefined}
      onClick={() => !editing && onSelect()}
      onKeyDown={(event) => {
        if (!editing && (event.key === 'Enter' || event.key === ' ')) {
          event.preventDefault();
          onSelect();
        }
      }}
      className={cn(
        'group rounded-lg px-2.5 py-2 outline-none transition-colors',
        'cursor-pointer hover:bg-background-tertiary/70 focus-visible:ring-2 focus-visible:ring-border-primary',
        active && 'bg-background-tertiary'
      )}
    >
      <div className="flex min-w-0 items-center gap-2">
        <InlineEditText
          value={session.name}
          onSave={async (name) => {
            await acpRenameSession(session.id, name);
            window.dispatchEvent(
              new CustomEvent(AppEvents.SESSION_RENAMED, {
                detail: { sessionId: session.id, newName: name, userInitiated: true },
              })
            );
            onRenamed(name);
          }}
          placeholder={intl.formatMessage(i18n.untitled)}
          disabled={isStreaming}
          singleClickEdit={false}
          className="min-w-0 flex-1 truncate !px-0 !py-0 text-sm text-text-primary hover:bg-transparent"
          editClassName="!text-sm"
          onEditStart={() => setEditing(true)}
          onEditEnd={() => setEditing(false)}
        />
        <SessionIndicators
          isStreaming={isStreaming}
          hasUnread={status?.hasUnreadActivity ?? false}
          hasError={status?.streamState === 'error'}
        />
      </div>
      <div className="mt-1 flex min-w-0 items-center justify-between gap-2 text-[11px] text-text-tertiary">
        <span className="truncate">{session.modelId ?? session.providerId ?? 'Lumina'}</span>
        <span className="flex-none tabular-nums">{activityLabel}</span>
      </div>
    </div>
  );
}

export default function ProjectWorkbenchView() {
  const intl = useIntl();
  const { projectId } = useParams<{ projectId: string }>();
  const [searchParams] = useSearchParams();
  const navigate = useNavigate();
  const { extensionsList } = useConfig();
  const [snapshot, setSnapshot] = useState<ProjectSnapshot | null>(null);
  const [sessions, setSessions] = useState<SessionListItem[]>([]);
  const [statuses, setStatuses] = useState<Map<string, SessionStatus>>(new Map());
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);

  const selectedSessionId = searchParams.get('resumeSessionId') ?? undefined;

  const loadWorkspace = useCallback(async () => {
    if (!projectId) return;
    setLoading(true);
    try {
      const nextSnapshot = await acpProjectSnapshot(projectId);
      const nextSessions = await acpListProjectSessions(nextSnapshot.project.rootPath);
      setSnapshot(nextSnapshot);
      setSessions(nextSessions);
    } catch (error) {
      toast.error(`Unable to load project: ${errorMessage(error)}`);
    } finally {
      setLoading(false);
    }
  }, [projectId]);

  useEffect(() => {
    void loadWorkspace();
  }, [loadWorkspace]);

  useEffect(() => {
    if (!selectedSessionId) return;
    window.dispatchEvent(
      new CustomEvent(AppEvents.ADD_ACTIVE_SESSION, {
        detail: { sessionId: selectedSessionId },
      })
    );
  }, [selectedSessionId]);

  useEffect(() => {
    const onStatusUpdate = (event: Event) => {
      const { sessionId, streamState } = (
        event as CustomEvent<{ sessionId: string; streamState: StreamState }>
      ).detail;
      setStatuses((previous) => {
        const existing = previous.get(sessionId);
        const next = new Map(previous);
        next.set(sessionId, {
          streamState,
          hasUnreadActivity:
            (existing?.streamState === 'streaming' && streamState === 'idle') ||
            existing?.hasUnreadActivity === true,
        });
        return next;
      });
    };

    const onSessionCreated = (event: Event) => {
      const session = (event as CustomEvent<{ session?: Session }>).detail?.session;
      if (!session) {
        void loadWorkspace();
        return;
      }
      if (session.project_id !== projectId) return;
      setSessions((previous) => {
        if (previous.some((item) => item.id === session.id)) return previous;
        return [sessionToListItem(session), ...previous];
      });
    };

    const onSessionRenamed = (event: Event) => {
      const { sessionId, newName } = (event as CustomEvent<{ sessionId: string; newName: string }>)
        .detail;
      setSessions((previous) =>
        previous.map((session) =>
          session.id === sessionId ? { ...session, name: newName, userSetName: true } : session
        )
      );
    };

    const onSessionDeleted = (event: Event) => {
      const { sessionId } = (event as CustomEvent<{ sessionId: string }>).detail;
      setSessions((previous) => previous.filter((session) => session.id !== sessionId));
      if (sessionId === selectedSessionId && projectId) {
        navigate(projectTaskPath(projectId), { replace: true });
      }
    };

    window.addEventListener(AppEvents.SESSION_STATUS_UPDATE, onStatusUpdate);
    window.addEventListener(AppEvents.SESSION_CREATED, onSessionCreated);
    window.addEventListener(AppEvents.SESSION_RENAMED, onSessionRenamed);
    window.addEventListener(AppEvents.SESSION_DELETED, onSessionDeleted);
    return () => {
      window.removeEventListener(AppEvents.SESSION_STATUS_UPDATE, onStatusUpdate);
      window.removeEventListener(AppEvents.SESSION_CREATED, onSessionCreated);
      window.removeEventListener(AppEvents.SESSION_RENAMED, onSessionRenamed);
      window.removeEventListener(AppEvents.SESSION_DELETED, onSessionDeleted);
    };
  }, [loadWorkspace, navigate, projectId, selectedSessionId]);

  const sortedSessions = useMemo(
    () =>
      [...sessions].sort(
        (left, right) =>
          new Date(sessionActivityAt(right)).getTime() - new Date(sessionActivityAt(left)).getTime()
      ),
    [sessions]
  );

  const selectSession = (sessionId: string) => {
    if (!projectId) return;
    window.dispatchEvent(
      new CustomEvent(AppEvents.ADD_ACTIVE_SESSION, {
        detail: { sessionId },
      })
    );
    setStatuses((previous) => {
      const existing = previous.get(sessionId);
      if (!existing?.hasUnreadActivity) return previous;
      const next = new Map(previous);
      next.set(sessionId, { ...existing, hasUnreadActivity: false });
      return next;
    });
    navigate(projectTaskPath(projectId, sessionId));
  };

  const createTask = async () => {
    if (!snapshot || creating) return;
    setCreating(true);
    try {
      const session = await createSession(snapshot.project.rootPath, {
        projectId: snapshot.project.id,
        allExtensions: extensionsList,
      });
      setSessions((previous) => [sessionToListItem(session), ...previous]);
      window.dispatchEvent(new CustomEvent(AppEvents.SESSION_CREATED, { detail: { session } }));
      selectSession(session.id);
    } catch (error) {
      toast.error(`Unable to create task: ${errorMessage(error)}`);
    } finally {
      setCreating(false);
    }
  };

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center gap-2 bg-background-primary pt-12 text-sm text-text-secondary">
        <Loader2 className="h-4 w-4 animate-spin" /> {intl.formatMessage(i18n.loading)}
      </div>
    );
  }

  if (!snapshot) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 bg-background-primary px-4 pt-12 text-center text-sm text-text-secondary">
        {intl.formatMessage(i18n.unavailable)}
        <Button variant="outline" size="sm" onClick={() => navigate('/projects')}>
          {intl.formatMessage(i18n.back)}
        </Button>
      </div>
    );
  }

  return (
    <aside className="flex h-full min-h-0 flex-col bg-background-primary pt-12">
      <header className="border-b border-border-secondary px-3 pb-3">
        <div className="flex min-w-0 items-center gap-2">
          <Button
            variant="ghost"
            size="xs"
            shape="round"
            onClick={() => navigate('/projects')}
            aria-label={intl.formatMessage(i18n.back)}
            title={intl.formatMessage(i18n.back)}
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div className="min-w-0 flex-1">
            <div className="flex min-w-0 items-center gap-1.5">
              <FolderGit2 className="h-4 w-4 flex-none text-text-secondary" />
              <h1 className="truncate text-sm font-semibold text-text-primary">
                {snapshot.project.name}
              </h1>
              {snapshot.project.profile.gitRepository && (
                <GitBranch className="h-3.5 w-3.5 flex-none text-text-tertiary" />
              )}
            </div>
            <p
              className="mt-0.5 truncate text-[11px] text-text-tertiary"
              title={snapshot.project.rootPath}
            >
              {snapshot.project.rootPath}
            </p>
          </div>
        </div>

        <Button
          className="mt-3 w-full"
          size="sm"
          onClick={() => void createTask()}
          disabled={creating}
        >
          {creating ? (
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
          ) : (
            <Plus className="mr-2 h-4 w-4" />
          )}
          {intl.formatMessage(creating ? i18n.creating : i18n.newTask)}
        </Button>
      </header>

      <div className="flex min-h-0 flex-1 flex-col">
        <div className="flex items-center justify-between px-4 pb-1 pt-3">
          <h2 className="text-[11px] font-semibold uppercase tracking-[0.14em] text-text-secondary">
            {intl.formatMessage(i18n.tasks)}
          </h2>
          <Button
            variant="ghost"
            size="xs"
            shape="round"
            onClick={() => void loadWorkspace()}
            aria-label={intl.formatMessage(i18n.refresh)}
            title={intl.formatMessage(i18n.refresh)}
          >
            <RefreshCw className="h-3.5 w-3.5" />
          </Button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
          {sortedSessions.length === 0 ? (
            <p className="px-2 py-4 text-xs leading-5 text-text-tertiary">
              {intl.formatMessage(i18n.empty)}
            </p>
          ) : (
            <div className="space-y-0.5">
              {sortedSessions.map((session) => (
                <ProjectSessionRow
                  key={session.id}
                  session={session}
                  active={session.id === selectedSessionId}
                  status={statuses.get(session.id)}
                  onSelect={() => selectSession(session.id)}
                  onRenamed={(name) =>
                    setSessions((previous) =>
                      previous.map((item) =>
                        item.id === session.id ? { ...item, name, userSetName: true } : item
                      )
                    )
                  }
                />
              ))}
            </div>
          )}
        </div>
      </div>
    </aside>
  );
}
