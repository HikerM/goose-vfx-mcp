import type { ReactNode } from 'react';
import type { IntlShape } from 'react-intl';
import { AlertTriangle, CheckCircle2, Info, LoaderCircle, RefreshCw } from 'lucide-react';
import { Button } from '../ui/button';
import { cn } from '../../utils';
import type { McpCredentialStatus, McpPlatformRecoveryViewModel } from '../../acp/mcp-platform';
import { useIntl } from '../../i18n';
import { mcpCenterMessages as messages } from './messages';

export function formatMcpValue(intl: IntlShape, value: string): string {
  return intl.formatMessage(messages.valueLabel, { value });
}

export function StatusBadge({
  children,
  tone = 'neutral',
  title,
  ariaLabel,
  className,
}: {
  children: ReactNode;
  tone?: 'neutral' | 'success' | 'warning' | 'danger' | 'info';
  title?: string;
  ariaLabel?: string;
  className?: string;
}) {
  const tones = {
    neutral: 'border-border-secondary text-text-secondary bg-background-secondary',
    success: 'border-border-success text-text-success bg-background-success/10',
    warning: 'border-border-warning text-text-warning bg-background-warning/10',
    danger: 'border-border-danger text-text-danger bg-background-danger/10',
    info: 'border-border-info text-text-info bg-background-info/10',
  };
  return (
    <span
      className={cn(
        'inline-flex max-w-full break-words rounded-full border px-2 py-0.5 text-xs leading-4 whitespace-normal',
        tones[tone],
        className
      )}
      title={title}
      aria-label={ariaLabel}
    >
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
        <p className="mt-1 max-w-lg break-words text-sm text-text-secondary">{description}</p>
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
          <p className="mt-1 break-words text-sm text-text-secondary">{localized.message}</p>
          <p className="mt-2 break-words text-sm text-text-primary">{localized.nextStep}</p>
          {recovery.correlationId && (
            <p className="mt-2 break-all font-mono text-xs text-text-tertiary">
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

export function NoticeBanner({
  tone = 'info',
  title,
  description,
  action,
  role,
}: {
  tone?: 'neutral' | 'success' | 'warning' | 'danger' | 'info';
  title: string;
  description: string;
  action?: ReactNode;
  role?: 'status' | 'alert';
}) {
  const icons = {
    neutral: RefreshCw,
    success: CheckCircle2,
    warning: AlertTriangle,
    danger: AlertTriangle,
    info: Info,
  } as const;
  const tones = {
    neutral: 'border-border-primary bg-background-secondary/30 text-text-primary',
    success: 'border-border-success bg-background-success/10 text-text-primary',
    warning: 'border-border-warning bg-background-warning/10 text-text-primary',
    danger: 'border-border-danger bg-background-danger/10 text-text-primary',
    info: 'border-border-info bg-background-info/10 text-text-primary',
  } as const;
  const Icon = icons[tone];

  return (
    <div
      role={role ?? (tone === 'danger' || tone === 'warning' ? 'alert' : 'status')}
      className={cn('rounded-xl border p-4', tones[tone])}
    >
      <div className="flex items-start gap-3">
        <Icon className="mt-0.5 h-5 w-5 shrink-0" />
        <div className="min-w-0 flex-1">
          <h3 className="font-medium">{title}</h3>
          <p className="mt-1 break-words text-sm text-text-secondary">{description}</p>
        </div>
        {action}
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
    title:
      recovery.title ||
      intl.formatMessage(
        mapped?.[0] ??
          (recovery.kind === 'monitor'
            ? messages.monitorFailedTitle
            : recovery.kind === 'connection'
              ? messages.recoveryConnectionTitle
              : messages.recoveryGenericTitle)
      ),
    message: recovery.message || intl.formatMessage(messages.requestFailedMessage),
    nextStep:
      recovery.nextStep ||
      intl.formatMessage(
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

type CredentialPresentation = {
  status: McpCredentialStatus | 'not_provided';
  badgeTone: 'neutral' | 'success' | 'warning' | 'danger' | 'info';
  badgeLabel: string;
  badgeAriaLabel: string;
  panelRole: 'status' | 'alert';
  panelTone: 'neutral' | 'success' | 'warning' | 'danger' | 'info';
  title: string;
  description: string;
  nextStep?: string;
  isReady: boolean;
};

type CredentialAttentionAction = {
  label: string;
  onClick?: () => void;
  disabled?: boolean;
};

function isCredentialStatus(value: unknown): value is McpCredentialStatus {
  return (
    value === 'unconfigured' ||
    value === 're_registration_required' ||
    value === 'trusted_state_conflict' ||
    value === 'temporarily_unavailable' ||
    value === 'ready'
  );
}

export function credentialPresentation(
  intl: IntlShape,
  status?: McpCredentialStatus | string | null
): CredentialPresentation {
  const resolvedStatus = isCredentialStatus(status) ? status : 'not_provided';
  switch (resolvedStatus) {
    case 'ready':
      return {
        status: resolvedStatus,
        badgeTone: 'success',
        badgeLabel: intl.formatMessage(messages.credentialReadyBadge),
        badgeAriaLabel: intl.formatMessage(messages.credentialReadyAria),
        panelRole: 'status',
        panelTone: 'success',
        title: intl.formatMessage(messages.credentialReadyTitle),
        description: intl.formatMessage(messages.credentialReadyDescription),
        isReady: true,
      };
    case 'unconfigured':
      return {
        status: resolvedStatus,
        badgeTone: 'warning',
        badgeLabel: intl.formatMessage(messages.credentialUnconfiguredBadge),
        badgeAriaLabel: intl.formatMessage(messages.credentialUnconfiguredAria),
        panelRole: 'alert',
        panelTone: 'warning',
        title: intl.formatMessage(messages.credentialUnconfiguredTitle),
        description: intl.formatMessage(messages.credentialUnconfiguredDescription),
        nextStep: intl.formatMessage(messages.credentialEnrollmentUnavailable),
        isReady: false,
      };
    case 're_registration_required':
      return {
        status: resolvedStatus,
        badgeTone: 'warning',
        badgeLabel: intl.formatMessage(messages.credentialReregistrationBadge),
        badgeAriaLabel: intl.formatMessage(messages.credentialReregistrationAria),
        panelRole: 'alert',
        panelTone: 'warning',
        title: intl.formatMessage(messages.credentialReregistrationTitle),
        description: intl.formatMessage(messages.credentialReregistrationDescription),
        nextStep: intl.formatMessage(messages.credentialEnrollmentUnavailable),
        isReady: false,
      };
    case 'trusted_state_conflict':
      return {
        status: resolvedStatus,
        badgeTone: 'danger',
        badgeLabel: intl.formatMessage(messages.credentialConflictBadge),
        badgeAriaLabel: intl.formatMessage(messages.credentialConflictAria),
        panelRole: 'alert',
        panelTone: 'danger',
        title: intl.formatMessage(messages.credentialConflictTitle),
        description: intl.formatMessage(messages.credentialConflictDescription),
        nextStep: intl.formatMessage(messages.credentialEnrollmentUnavailable),
        isReady: false,
      };
    case 'temporarily_unavailable':
      return {
        status: resolvedStatus,
        badgeTone: 'info',
        badgeLabel: intl.formatMessage(messages.credentialUnavailableBadge),
        badgeAriaLabel: intl.formatMessage(messages.credentialUnavailableAria),
        panelRole: 'status',
        panelTone: 'info',
        title: intl.formatMessage(messages.credentialUnavailableTitle),
        description: intl.formatMessage(messages.credentialUnavailableDescription),
        nextStep: intl.formatMessage(messages.credentialUnavailableNextStep),
        isReady: false,
      };
    default:
      return {
        status: 'not_provided',
        badgeTone: 'neutral',
        badgeLabel: intl.formatMessage(messages.credentialUnknownBadge),
        badgeAriaLabel: intl.formatMessage(messages.credentialUnknownAria),
        panelRole: 'status',
        panelTone: 'neutral',
        title: intl.formatMessage(messages.credentialUnknownTitle),
        description: intl.formatMessage(messages.credentialUnknownDescription),
        nextStep: intl.formatMessage(messages.credentialUnknownNextStep),
        isReady: false,
      };
  }
}

export function CredentialStatusBadge({
  status,
}: {
  status?: McpCredentialStatus | string | null;
}) {
  const intl = useIntl();
  const presentation = credentialPresentation(intl, status);
  return (
    <StatusBadge
      tone={presentation.badgeTone}
      ariaLabel={presentation.badgeAriaLabel}
      title={presentation.badgeAriaLabel}
      className="min-w-0"
    >
      <span className="truncate">{presentation.badgeLabel}</span>
    </StatusBadge>
  );
}

export function CredentialAttentionPanel({
  status,
  actions = [],
  mode = 'full',
}: {
  status?: McpCredentialStatus | string | null;
  actions?: CredentialAttentionAction[];
  mode?: 'full' | 'note';
}) {
  const intl = useIntl();
  const presentation = credentialPresentation(intl, status);
  const tones = {
    neutral: 'border-border-primary bg-background-secondary/30',
    success: 'border-border-success bg-background-success/10',
    warning: 'border-border-warning bg-background-warning/10',
    danger: 'border-border-danger bg-background-danger/10',
    info: 'border-border-info bg-background-info/10',
  } as const;

  return (
    <div
      role={presentation.panelRole}
      className={cn(
        'rounded-xl border p-4',
        tones[presentation.panelTone],
        mode === 'note' ? 'space-y-2' : 'space-y-3'
      )}
    >
      <div className="space-y-1">
        <div className="flex flex-wrap items-center gap-2">
          <h3 className="font-medium text-text-primary">{presentation.title}</h3>
          <CredentialStatusBadge status={presentation.status} />
        </div>
        <p className="text-sm text-text-secondary">{presentation.description}</p>
        {presentation.nextStep && (
          <p className="text-sm text-text-primary">{presentation.nextStep}</p>
        )}
      </div>

      {mode === 'full' && !presentation.isReady && (
        <div className="flex flex-wrap gap-2">
          {actions.map((action) => (
            <Button
              key={action.label}
              variant="outline"
              size="sm"
              disabled={action.disabled}
              onClick={action.onClick}
            >
              {action.label}
            </Button>
          ))}
          <Button
            variant="outline"
            size="sm"
            disabled
            aria-label={intl.formatMessage(messages.credentialEnrollmentButton)}
            title={intl.formatMessage(messages.credentialEnrollmentButton)}
          >
            {intl.formatMessage(messages.credentialEnrollmentButton)}
          </Button>
        </div>
      )}
    </div>
  );
}
