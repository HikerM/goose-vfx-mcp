import { cn } from '../../utils';
import type { ProjectRunStatus, WorkItemStatus } from '../../acp/projects';

const statusTone: Record<WorkItemStatus | ProjectRunStatus, string> = {
  draft: 'bg-background-tertiary text-text-secondary',
  planned: 'bg-background-tertiary text-text-primary',
  pending: 'bg-background-tertiary text-text-primary',
  running: 'bg-blue-500/10 text-blue-700 dark:text-blue-300',
  validating: 'bg-violet-500/10 text-violet-700 dark:text-violet-300',
  review: 'bg-amber-500/10 text-amber-700 dark:text-amber-300',
  succeeded: 'bg-green-500/10 text-green-700 dark:text-green-300',
  completed: 'bg-green-500/10 text-green-700 dark:text-green-300',
  failed: 'bg-red-500/10 text-red-700 dark:text-red-300',
  cancelled: 'bg-background-tertiary text-text-secondary',
  interrupted: 'bg-orange-500/10 text-orange-700 dark:text-orange-300',
  blocked: 'bg-red-500/10 text-red-700 dark:text-red-300',
};

export function ProjectStatusBadge({ status }: { status: WorkItemStatus | ProjectRunStatus }) {
  return (
    <span
      className={cn(
        'inline-flex rounded-full px-2 py-0.5 text-[11px] font-semibold capitalize',
        statusTone[status]
      )}
    >
      {status.replace(/_/g, ' ')}
    </span>
  );
}
