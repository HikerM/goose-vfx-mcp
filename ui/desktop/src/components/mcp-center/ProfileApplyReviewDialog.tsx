import type { ReactNode } from 'react';
import type { IntlShape } from 'react-intl';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../ui/dialog';
import { Button } from '../ui/button';
import {
  DefinitionList,
  RecoveryPanel,
  StatePanel,
  StatusBadge,
  formatMcpValue,
} from './McpCenterCommon';
import {
  type McpPlatformRecoveryViewModel,
  type McpProfileApplyEntry,
} from '../../acp/mcp-platform';
import { mcpCenterMessages as messages } from './messages';
import { useIntl } from '../../i18n';

export type ProfileApplyDialogPhase =
  | 'review'
  | 'resolving-session'
  | 'confirming'
  | 'creating-session'
  | 'success'
  | 'error';

export type ProfileApplyReviewDialogState = {
  profileId: string;
  profileName: string;
  review: PublicProfileApplyReview;
  phase: ProfileApplyDialogPhase;
  confirmationTokenAvailable: boolean;
  recovery: McpPlatformRecoveryViewModel | null;
  canRecreatePlan: boolean;
};

export type PublicProfileApplyReviewEntry = Pick<
  McpProfileApplyEntry,
  'managedMcpId' | 'mcpId' | 'name' | 'version' | 'health' | 'authReady' | 'policyReady' | 'readiness'
>;

export type PublicProfileApplyReview = {
  profileId: string;
  profileRevision: number;
  mergePolicy: string;
  entries: PublicProfileApplyReviewEntry[];
  expiresAtMs: number;
};

export function ProfileApplyReviewDialog({
  state,
  onClose,
  onConfirm,
  onRecreatePlan,
}: {
  state: ProfileApplyReviewDialogState | null;
  onClose: () => void;
  onConfirm: () => void;
  onRecreatePlan: () => void;
}) {
  const intl = useIntl();

  if (!state) {
    return null;
  }

  const { review, phase, confirmationTokenAvailable, recovery, canRecreatePlan, profileName } =
    state;
  const pending = phase === 'confirming' || phase === 'creating-session';
  const isExpired = review.expiresAtMs <= Date.now();
  const readyEntries = review.entries.filter(
    (entry) => entry.authReady && entry.policyReady && entry.readiness === 'complete'
  ).length;
  const hasPartialReadiness = readyEntries > 0 && readyEntries < review.entries.length;
  const hasBlockedEntries = readyEntries < review.entries.length;
  const showRecreatePlan = canRecreatePlan || isExpired;
  const canConfirm =
    phase === 'review' && confirmationTokenAvailable && !isExpired && !pending;

  const statusBadge = isExpired ? (
    <StatusBadge tone="danger">
      {intl.formatMessage(messages.profileApplyExpiredBadge)}
    </StatusBadge>
  ) : hasBlockedEntries ? (
    <StatusBadge tone="warning">
      {intl.formatMessage(messages.profileApplyPartialBadge)}
    </StatusBadge>
  ) : (
    <StatusBadge tone="success">
      {intl.formatMessage(messages.profileApplyReadyBadge)}
    </StatusBadge>
  );

  return (
    <Dialog open onOpenChange={(open) => !open && !pending && onClose()}>
      <DialogContent
        className="flex max-h-[90vh] w-[min(100%,72rem)] max-w-[calc(100%-1.5rem)] flex-col overflow-hidden p-0 sm:max-w-[72rem]"
        onEscapeKeyDown={(event) => {
          if (pending) event.preventDefault();
        }}
        onInteractOutside={(event) => {
          if (pending) event.preventDefault();
        }}
        aria-label={intl.formatMessage(messages.profileApplyReviewTitle)}
      >
        <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
          <DialogHeader className="border-b border-border-primary px-5 pt-5 sm:px-6">
            <div className="flex flex-wrap items-start justify-between gap-3 pr-8">
              <div className="min-w-0 space-y-1">
                <DialogTitle className="truncate">
                  {intl.formatMessage(messages.profileApplyReviewTitle)}
                </DialogTitle>
                <DialogDescription className="max-w-3xl">
                  {intl.formatMessage(messages.profileApplyReviewDescription)}
                </DialogDescription>
              </div>
              <div className="flex flex-wrap gap-2">
                {statusBadge}
                <StatusBadge tone="info">
                  {intl.formatMessage(messages.profileApplyOpenNewSession)}
                </StatusBadge>
              </div>
            </div>
          </DialogHeader>

          <div className="min-h-0 flex-1 overflow-y-auto px-5 py-5 sm:px-6">
            <div className="space-y-5">
              <section className="space-y-4">
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div className="min-w-0">
                    <h2 className="truncate text-xl font-medium text-text-primary" title={profileName}>
                      {profileName}
                    </h2>
                    <p className="mt-2 max-w-3xl text-sm text-text-secondary">
                      {intl.formatMessage(messages.profileApplySafeBoundary)}
                    </p>
                  </div>
                </div>

                <DefinitionList
                  items={[
                    [
                      intl.formatMessage(messages.profileApplyExpiresAt),
                      `${intl.formatDate(review.expiresAtMs, {
                        year: 'numeric',
                        month: 'short',
                        day: 'numeric',
                      })} ${intl.formatTime(review.expiresAtMs, {
                        hour: 'numeric',
                        minute: '2-digit',
                      })}`,
                    ],
                    [
                      intl.formatMessage(messages.profileApplyMergePolicy),
                      formatKnownOrSafeValue(intl, review.mergePolicy),
                    ],
                    [
                      intl.formatMessage(messages.profileEntriesLabel),
                      intl.formatMessage(messages.profileEntriesCount, {
                        count: review.entries.length,
                      }),
                    ],
                    [
                      intl.formatMessage(messages.credentialStatusLabel),
                      hasBlockedEntries
                        ? intl.formatMessage(messages.profileApplyPartialBadge)
                        : intl.formatMessage(messages.profileApplyReadyBadge),
                    ],
                  ]}
                />

                {isExpired && (
                  <StatePanel
                    kind="error"
                    title={intl.formatMessage(messages.profileApplyExpiredTitle)}
                    description={intl.formatMessage(messages.profileApplyExpiredDescription)}
                  />
                )}

                {phase === 'confirming' && (
                  <StatePanel
                    kind="loading"
                    title={intl.formatMessage(messages.profileApplyConfirming)}
                    description={intl.formatMessage(messages.profileApplyStatusConfirming)}
                  />
                )}

                {phase === 'resolving-session' && (
                  <StatePanel
                    kind="loading"
                    title={intl.formatMessage(messages.profileApplyResolvingSession)}
                    description={intl.formatMessage(messages.profileApplyStatusResolvingSession)}
                  />
                )}

                {phase === 'creating-session' && (
                  <StatePanel
                    kind="loading"
                    title={intl.formatMessage(messages.profileApplyCreatingSession)}
                    description={intl.formatMessage(messages.profileApplyStatusCreatingSession)}
                  />
                )}

                {phase === 'success' && (
                  <StatePanel
                    kind="success"
                    title={intl.formatMessage(messages.profileApplySuccessTitle)}
                    description={intl.formatMessage(messages.profileApplySuccessDescription)}
                  />
                )}

                {recovery && <RecoveryPanel recovery={recovery} />}
              </section>

              <section className="space-y-3" aria-label={intl.formatMessage(messages.profileApplyEntriesLabel)}>
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <h3 className="font-medium text-text-primary">
                    {intl.formatMessage(messages.profileApplyEntriesLabel)}
                  </h3>
                  {hasPartialReadiness && (
                    <StatusBadge tone="warning">
                      {intl.formatMessage(messages.profileApplyPartialBadge)}
                    </StatusBadge>
                  )}
                </div>

                {review.entries.length === 0 ? (
                  <StatePanel
                    kind="empty"
                    title={intl.formatMessage(messages.profileApplyEntriesEmpty)}
                    description={intl.formatMessage(messages.profileApplyEntriesEmptyDescription)}
                  />
                ) : (
                  <div className="grid gap-3 lg:grid-cols-2">
                    {review.entries.map((entry, index) => {
                      const identity = entry.name || entry.mcpId || entry.managedMcpId;
                      return (
                        <article
                          key={`${entry.managedMcpId}:${index}`}
                          className="min-w-0 rounded-xl border border-border-primary bg-background-secondary/20 p-4"
                        >
                          <div className="flex flex-wrap items-start justify-between gap-2">
                            <div className="min-w-0">
                              <h4 className="truncate font-medium text-text-primary" title={identity}>
                                {identity}
                              </h4>
                              <p className="mt-1 break-words text-sm text-text-secondary">
                                {entry.mcpId || sanitizeSafeText(entry.managedMcpId)}
                              </p>
                              {entry.version && (
                                <p className="mt-1 text-xs text-text-tertiary">{entry.version}</p>
                              )}
                            </div>
                            <StatusBadge
                              tone={entry.readiness === 'complete' ? 'success' : 'warning'}
                            >
                              {formatKnownOrSafeValue(intl, entry.readiness)}
                            </StatusBadge>
                          </div>

                          <div className="mt-3 flex flex-wrap gap-1.5">
                            <StatusBadge
                              tone={entry.health === 'healthy' ? 'success' : 'warning'}
                            >
                              {formatKnownOrSafeValue(intl, entry.health)}
                            </StatusBadge>
                            <StatusBadge tone={entry.authReady ? 'success' : 'warning'}>
                              {intl.formatMessage(
                                entry.authReady
                                  ? messages.profileApplyAuthReady
                                  : messages.profileApplyAuthBlocked
                              )}
                            </StatusBadge>
                            <StatusBadge tone={entry.policyReady ? 'success' : 'warning'}>
                              {intl.formatMessage(
                                entry.policyReady
                                  ? messages.profileApplyPolicyReady
                                  : messages.profileApplyPolicyBlocked
                              )}
                            </StatusBadge>
                          </div>
                        </article>
                      );
                    })}
                  </div>
                )}
              </section>
            </div>
          </div>
        </div>

        <DialogFooter className="border-t border-border-primary px-5 py-4 sm:px-6">
          <Button variant="outline" disabled={pending} onClick={onClose}>
            {intl.formatMessage(messages.cancel)}
          </Button>
          {showRecreatePlan && (
            <Button variant="outline" disabled={pending} onClick={onRecreatePlan}>
              {intl.formatMessage(messages.profileApplyRestart)}
            </Button>
          )}
          <Button disabled={!canConfirm} onClick={onConfirm}>
            {intl.formatMessage(
              phase === 'resolving-session'
                ? messages.profileApplyResolvingSession
                : phase === 'confirming'
                ? messages.profileApplyConfirming
                : phase === 'creating-session'
                  ? messages.profileApplyCreatingSession
                  : messages.profileApplyConfirm
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function sanitizeSafeText(value: string | null | undefined): string {
  if (!value) return '';
  return value.replace(/[\p{Cc}\p{Cf}]+/gu, ' ').trim();
}

function formatKnownOrSafeValue(
  intl: IntlShape,
  value: string
): ReactNode {
  const fallback = intl.formatMessage(messages.valueLabel, { value: '__unknown__' });
  const formatted = formatMcpValue(intl, value);
  if (formatted !== fallback) {
    return formatted;
  }

  const safe = sanitizeSafeText(value);
  return safe ? safe.replace(/_/g, ' ') : fallback;
}
