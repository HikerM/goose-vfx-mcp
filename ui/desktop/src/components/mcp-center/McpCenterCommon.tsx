import type { ReactNode } from 'react';
import type { IntlShape } from 'react-intl';
import { AlertTriangle, CheckCircle2, LoaderCircle, RefreshCw } from 'lucide-react';
import { Button } from '../ui/button';
import { cn } from '../../utils';
import type { McpPlatformRecoveryViewModel } from '../../acp/mcp-platform';
import { useIntl } from '../../i18n';
import { mcpCenterMessages as messages } from './messages';

export function formatMcpValue(intl: IntlShape, value: string): string {
  return intl.formatMessage(messages.valueLabel, { value });
}

export function StatusBadge({
  children,
  tone = 'neutral',
}: {
  children: ReactNode;
  tone?: 'neutral' | 'success' | 'warning' | 'danger' | 'info';
}) {
  const tones = {
    neutral: 'border-border-secondary text-text-secondary bg-background-secondary',
    success: 'border-border-success text-text-success bg-background-success/10',
    warning: 'border-border-warning text-text-warning bg-background-warning/10',
    danger: 'border-border-danger text-text-danger bg-background-danger/10',
    info: 'border-border-info text-text-info bg-background-info/10',
  };
  return (
    <span className={cn('inline-flex rounded-full border px-2 py-0.5 text-xs', tones[tone])}>
      {children}
    </span>
  );
}

export function StatePanel({
  kind,
  title,
  description,
  action,
}: {
  kind: 'loading' | 'empty' | 'error' | 'success';
  title: string;
  description: string;
  action?: ReactNode;
}) {
  const Icon =
    kind === 'loading'
      ? LoaderCircle
      : kind === 'error'
        ? AlertTriangle
        : kind === 'success'
          ? CheckCircle2
          : RefreshCw;
  return (
    <div
      role={kind === 'error' ? 'alert' : 'status'}
      className="flex min-h-40 flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border-primary bg-background-secondary/30 p-6 text-center"
    >
      <Icon className={cn('h-6 w-6 text-text-secondary', kind === 'loading' && 'animate-spin')} />
      <div>
        <h3 className="font-medium text-text-primary">{title}</h3>
        <p className="mt-1 max-w-lg text-sm text-text-secondary">{description}</p>
      </div>
      {action}
    </div>
  );
}

export function RecoveryPanel({
  recovery,
  onRetry,
}: {
  recovery: McpPlatformRecoveryViewModel;
  onRetry?: () => void;
}) {
  const intl = useIntl();
  const localized = recoveryMessages(intl, recovery);
  return (
    <div role="alert" className="rounded-xl border border-border-danger bg-background-danger/5 p-4">
      <div className="flex items-start gap-3">
        <AlertTriangle className="mt-0.5 h-5 w-5 shrink-0 text-text-danger" />
        <div className="min-w-0 flex-1">
          <h3 className="font-medium text-text-primary">{localized.title}</h3>
          <p className="mt-1 text-sm text-text-secondary">{localized.message}</p>
          <p className="mt-2 text-sm text-text-primary">{localized.nextStep}</p>
          {recovery.correlationId && (
            <p className="mt-2 truncate font-mono text-xs text-text-tertiary">
              {intl.formatMessage(messages.reference, { value: recovery.correlationId })}
            </p>
          )}
        </div>
        {onRetry && recovery.retryable && (
          <Button variant="outline" size="sm" onClick={onRetry}>
            {intl.formatMessage(messages.retry)}
          </Button>
        )}
      </div>
    </div>
  );
}

function recoveryMessages(
  intl: IntlShape,
  recovery: McpPlatformRecoveryViewModel
): Pick<McpPlatformRecoveryViewModel, 'title' | 'message' | 'nextStep'> {
  const byCode = {
    credential_missing: [messages.recoveryCredentialTitle, messages.recoveryCredentialNext],
    remote_http_policy_unavailable: [
      messages.recoveryRemotePolicyTitle,
      messages.recoveryRemotePolicyNext,
    ],
    manual_stdio_provider_unavailable: [messages.recoveryStdioTitle, messages.recoveryStdioNext],
    policy_denied: [messages.recoveryPolicyTitle, messages.recoveryPolicyNext],
    plan_expired: [messages.recoveryPlanExpiredTitle, messages.recoveryPlanExpiredNext],
    plan_stale: [messages.recoveryPlanStaleTitle, messages.recoveryPlanStaleNext],
    revision_conflict: [messages.recoveryRevisionTitle, messages.recoveryRevisionNext],
    repository_unavailable: [messages.recoveryRepositoryTitle, messages.recoveryRepositoryNext],
  } as const;
  const mapped = recovery.code ? byCode[recovery.code as keyof typeof byCode] : undefined;
  return {
    title: intl.formatMessage(
      mapped?.[0] ??
        (recovery.kind === 'monitor'
          ? messages.monitorFailedTitle
          : recovery.kind === 'connection'
            ? messages.recoveryConnectionTitle
            : messages.recoveryGenericTitle)
    ),
    message: intl.formatMessage(messages.requestFailedMessage),
    nextStep: intl.formatMessage(
      mapped?.[1] ??
        (recovery.kind === 'monitor'
          ? messages.monitorFailedNext
          : recovery.kind === 'connection'
            ? messages.recoveryConnectionNext
            : recovery.retryable
              ? messages.recoveryGenericRetry
              : messages.recoveryGenericReview)
    ),
  };
}

export function DefinitionList({ items }: { items: Array<[string, ReactNode]> }) {
  return (
    <dl className="grid grid-cols-1 gap-x-6 gap-y-3 text-sm sm:grid-cols-2">
      {items.map(([label, value]) => (
        <div key={label} className="min-w-0">
          <dt className="text-text-secondary">{label}</dt>
          <dd className="mt-0.5 break-words text-text-primary">{value}</dd>
        </div>
      ))}
    </dl>
  );
}
