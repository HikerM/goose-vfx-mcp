import { useState } from 'react';
import type { IntlShape } from 'react-intl';
import type { McpPlanReview, McpTaskRef } from '@aaif/goose-sdk';
import type { McpHttpsProvisionPlanReview } from '../../acp/mcp-platform';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../ui/dialog';
import { Button } from '../ui/button';
import { DefinitionList, formatMcpValue, RecoveryPanel, StatusBadge } from './McpCenterCommon';
import { readCatalogTarget } from './catalogIdentity';
import {
  confirmMcpPlan,
  toMcpRecoveryViewModel,
  type McpPlatformRecoveryViewModel,
} from '../../acp/mcp-platform';
import { mcpCenterMessages as messages } from './messages';
import { useIntl } from '../../i18n';

export function PlanReviewDialog({
  plan,
  operation,
  onClose,
  onTaskCreated,
  httpsPlan,
}: {
  plan: McpPlanReview | null;
  httpsPlan?: McpHttpsProvisionPlanReview | null;
  operation: McpTaskRef['operation'] | 'provision' | null;
  onClose: () => void;
  onTaskCreated: (task: McpTaskRef) => void;
}) {
  const intl = useIntl();
  const [submitting, setSubmitting] = useState<'confirm' | 'reject' | null>(null);
  const [error, setError] = useState<McpPlatformRecoveryViewModel | null>(null);
  const catalogTarget = plan ? readCatalogTarget(plan) : null;

  const decide = async (decision: 'confirm' | 'reject') => {
    if (httpsPlan) {
      if (decision !== 'confirm') { onClose(); return; }
      setSubmitting('confirm');
      setError(null);
      try {
        const task = await confirmMcpPlan(httpsPlan, 'confirm');
        onTaskCreated(task);
        onClose();
      } catch (cause) { setError(toMcpRecoveryViewModel(cause)); }
      finally { setSubmitting(null); }
      return;
    }
    if (!plan) return;
    setSubmitting(decision);
    setError(null);
    try {
      const task = await confirmMcpPlan(plan, decision);
      if (decision === 'confirm') onTaskCreated(task);
      onClose();
    } catch (cause) {
      setError(toMcpRecoveryViewModel(cause));
    } finally {
      setSubmitting(null);
    }
  };

  return (
    <Dialog open={!!plan || !!httpsPlan} onOpenChange={(open) => !open && !submitting && onClose()}>
      <DialogContent className="max-h-[min(760px,calc(100vh-32px))] max-w-3xl overflow-hidden">
        {httpsPlan ? (
          <div className="flex min-h-0 flex-col gap-6">
            <DialogHeader><DialogTitle>{intl.formatMessage(messages.reviewPlan)}</DialogTitle><DialogDescription>{intl.formatMessage(messages.reviewPlanDescription)}</DialogDescription></DialogHeader>
            <div className="min-h-0 space-y-4 overflow-y-auto pr-1">
              <DefinitionList items={[[intl.formatMessage(messages.mcp), httpsPlan.mcp.name], [intl.formatMessage(messages.version), httpsPlan.mcp.version], [intl.formatMessage(messages.operation), formatMcpValue(intl, httpsPlan.operation)], [intl.formatMessage(messages.writesFiles), intl.formatMessage(httpsPlan.effects.file.writesFiles ? messages.yes : messages.no)], [intl.formatMessage(messages.removesFiles), intl.formatMessage(httpsPlan.effects.file.removesFiles ? messages.yes : messages.no)], [intl.formatMessage(messages.ownedItems), httpsPlan.effects.file.ownedItems], [intl.formatMessage(messages.startsDuringConfirmation), intl.formatMessage(httpsPlan.effects.process.startsDuringConfirmation ? messages.yes : messages.no)]]} />
              <p className="break-words rounded-lg bg-background-secondary/40 p-3 text-sm text-text-secondary">{intl.formatMessage(messages.planBoundaryNote)}</p>
              {httpsPlan.warnings.length > 0 && (
                <section aria-label={intl.formatMessage(messages.warnings)}>
                  <h3 className="mb-2 font-medium text-text-primary">
                    {intl.formatMessage(messages.warnings)}
                  </h3>
                  <ul className="space-y-2 text-sm">
                    {httpsPlan.warnings.map((warning, index) => (
                      <li
                        key={`https-warning-${index}`}
                        className="rounded-lg border border-border-warning bg-background-warning/10 p-3 text-text-warning"
                      >
                        {warning}
                      </li>
                    ))}
                  </ul>
                </section>
              )}
              {error && <RecoveryPanel recovery={error} />}
            </div>
            <DialogFooter className="sticky bottom-0 border-t border-border-primary bg-background-primary pt-4"><Button variant="outline" disabled={!!submitting} onClick={() => void decide('reject')}>{intl.formatMessage(messages.rejectPlan)}</Button><Button disabled={!!submitting || httpsPlan.policy.outcome === 'deny'} onClick={() => void decide('confirm')}>{intl.formatMessage(submitting === 'confirm' ? messages.confirming : messages.confirmStart)}</Button></DialogFooter>
          </div>
        ) : plan && (
          <div className="flex min-h-0 flex-col gap-6">
            <DialogHeader className="shrink-0">
              <DialogTitle>{intl.formatMessage(messages.reviewPlan)}</DialogTitle>
              <DialogDescription>
                {intl.formatMessage(messages.reviewPlanDescription)}
              </DialogDescription>
            </DialogHeader>

            <div className="min-h-0 space-y-5 overflow-y-auto pr-1">
              {error && <RecoveryPanel recovery={error} />}

              <section className="space-y-4" aria-label={intl.formatMessage(messages.planImpact)}>
                <div className="flex flex-wrap items-center gap-2">
                  <h3 className="break-words text-lg font-medium text-text-primary">{plan.name}</h3>
                  <StatusBadge>{plan.version}</StatusBadge>
                  <StatusBadge>{formatMcpValue(intl, plan.trustTier)}</StatusBadge>
                  <StatusBadge tone={plan.policy.outcome === 'deny' ? 'danger' : 'info'}>
                    {intl.formatMessage(messages.policy)}:{' '}
                    {formatMcpValue(intl, plan.policy.outcome)}
                  </StatusBadge>
                </div>
                <DefinitionList
                  items={[
                    [intl.formatMessage(messages.operation), formatMcpValue(intl, operation ?? '')],
                    [intl.formatMessage(messages.source), plan.sourceId],
                    [
                      intl.formatMessage(messages.catalogItem),
                      catalogTarget
                        ? `${catalogTarget.sourceId} / ${catalogTarget.mcpId} / ${catalogTarget.version}`
                        : intl.formatMessage(messages.noneDeclared),
                    ],
                    [intl.formatMessage(messages.publisher), plan.publisher.name],
                    [intl.formatMessage(messages.version), plan.version],
                    [intl.formatMessage(messages.manifestDigest), plan.selectedManifestDigest],
                    [intl.formatMessage(messages.trust), formatMcpValue(intl, plan.trustTier)],
                    [intl.formatMessage(messages.scope), intl.formatMessage(messages.currentUser)],
                    [
                      intl.formatMessage(messages.planExpiresAt),
                      `${intl.formatDate(plan.expiresAtMs, {
                        year: 'numeric',
                        month: 'short',
                        day: 'numeric',
                      })} ${intl.formatTime(plan.expiresAtMs, {
                        hour: 'numeric',
                        minute: '2-digit',
                      })}`,
                    ],
                    [
                      intl.formatMessage(messages.defaultState),
                      intl.formatMessage(plan.defaultDisabled ? messages.disabled : messages.enabled),
                    ],
                    [
                      intl.formatMessage(messages.reversible),
                      intl.formatMessage(plan.reversibility.reversible ? messages.yes : messages.no),
                    ],
                    [
                      intl.formatMessage(messages.writesFiles),
                      intl.formatMessage(plan.fileEffects.writesFiles ? messages.yes : messages.no),
                    ],
                    [
                      intl.formatMessage(messages.removesFiles),
                      intl.formatMessage(plan.fileEffects.removesFiles ? messages.yes : messages.no),
                    ],
                    [intl.formatMessage(messages.ownedItems), plan.fileEffects.ownedItems],
                    [
                      intl.formatMessage(messages.startsDuringConfirmation),
                      intl.formatMessage(
                        plan.processEffects.startsDuringConfirmation ? messages.yes : messages.no
                      ),
                    ],
                  ]}
                />
                <p className="break-words rounded-lg bg-background-secondary/40 p-3 text-sm text-text-secondary">
                  {intl.formatMessage(messages.planBoundaryNote)}
                </p>
              </section>

              {operation === 'uninstall' && (
                <p
                  role="alert"
                  className="rounded-lg border border-border-danger bg-background-danger/5 p-3 text-sm text-text-danger"
                >
                  {intl.formatMessage(messages.uninstallWarning)}
                </p>
              )}

              <section className="grid gap-4 md:grid-cols-3">
                <div className="min-w-0">
                  <h3 className="mb-2 font-medium text-text-primary">
                    {intl.formatMessage(messages.immutableEvidence)}
                  </h3>
                  <DefinitionList items={immutableEvidenceItems(plan, intl)} />
                </div>
                <div className="min-w-0">
                  <h3 className="mb-2 font-medium text-text-primary">
                    {intl.formatMessage(messages.networkOrigins)}
                  </h3>
                  {plan.networkOrigins.length === 0 ? (
                    <p className="text-sm text-text-secondary">
                      {intl.formatMessage(messages.noneDeclared)}
                    </p>
                  ) : (
                    <ul className="list-disc space-y-1 break-all pl-5 text-sm text-text-primary">
                      {plan.networkOrigins.map((origin) => (
                        <li key={origin}>{origin}</li>
                      ))}
                    </ul>
                  )}
                </div>
                <div className="min-w-0">
                  <h3 className="mb-2 font-medium text-text-primary">
                    {intl.formatMessage(messages.hostRegistrations)}
                  </h3>
                  {plan.hostEffects.registrationIds.length === 0 ? (
                    <p className="text-sm text-text-secondary">
                      {intl.formatMessage(messages.noneDeclared)}
                    </p>
                  ) : (
                    <ul className="list-disc space-y-1 break-all pl-5 text-sm text-text-primary">
                      {plan.hostEffects.registrationIds.map((registrationId) => (
                        <li key={registrationId}>{registrationId}</li>
                      ))}
                    </ul>
                  )}
                </div>
              </section>

              <section>
                <h3 className="mb-2 font-medium text-text-primary">
                  {intl.formatMessage(messages.requiredConfirmations)}
                </h3>
                {plan.requiredConfirmations.length === 0 ? (
                  <p className="text-sm text-text-secondary">
                    {intl.formatMessage(messages.noAdditionalConfirmations)}
                  </p>
                ) : (
                  <ul className="list-disc space-y-1 pl-5 text-sm text-text-primary">
                    {plan.requiredConfirmations.map((item, index) => (
                      <li key={`${item.type}-${index}`}>
                        <span className="font-medium">{formatMcpValue(intl, item.type)}</span>
                        {'reason_code' in item
                          ? ` · ${intl.formatMessage(messages.confirmationReasonCode)}: ${item.reason_code}`
                          : ` · ${intl.formatMessage(messages.confirmationPermissionId)}: ${item.permission_id}`}
                      </li>
                    ))}
                  </ul>
                )}
              </section>

              <section className="grid gap-4 md:grid-cols-2">
                <div className="min-w-0">
                  <h3 className="mb-2 font-medium text-text-primary">
                    {intl.formatMessage(messages.permissions)}
                  </h3>
                  {plan.permissions.length === 0 ? (
                    <p className="text-sm text-text-secondary">
                      {intl.formatMessage(messages.noPermissions)}
                    </p>
                  ) : (
                    <ul className="space-y-2 text-sm">
                      {plan.permissions.map((permission) => (
                        <li
                          key={permission.id}
                          className="rounded-lg border border-border-primary p-3"
                        >
                          <div className="font-medium text-text-primary">
                            {formatMcpValue(intl, permission.kind)}
                          </div>
                          <div className="mt-1 break-words text-text-secondary">
                            {permission.reason}
                          </div>
                          {permission.scope && (
                            <div className="mt-1 break-all font-mono text-xs">
                              {permission.scope}
                            </div>
                          )}
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
                <div className="min-w-0">
                  <h3 className="mb-2 font-medium text-text-primary">
                    {intl.formatMessage(messages.policyRollback)}
                  </h3>
                  <div className="space-y-2 rounded-lg border border-border-primary p-3 text-sm">
                    <p className="text-text-primary">
                      {intl.formatMessage(messages.recovery)}:{' '}
                      {formatMcpValue(intl, plan.reversibility.strategy.type)}
                    </p>
                    {plan.policy.reasons.map((reason) => (
                      <p key={reason.code} className="break-words text-text-secondary">
                        {reason.message}
                      </p>
                    ))}
                  </div>
                </div>
              </section>

              <section className="grid gap-4 md:grid-cols-2">
                <div className="min-w-0">
                  <h3 className="mb-2 font-medium text-text-primary">
                    {intl.formatMessage(messages.warnings)}
                  </h3>
                  {plan.warnings.length === 0 ? (
                    <p className="text-sm text-text-secondary">
                      {intl.formatMessage(messages.noWarnings)}
                    </p>
                  ) : (
                    <ul className="space-y-2 text-sm">
                      {plan.warnings.map((warning) => (
                        <li
                          key={warning}
                          className="rounded-lg border border-border-warning bg-background-warning/10 p-3 text-text-warning"
                        >
                          {formatMcpValue(intl, warning)}
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
                <div className="min-w-0">
                  <h3 className="mb-2 font-medium text-text-primary">
                    {intl.formatMessage(messages.policy)}
                  </h3>
                  <div className="space-y-2 rounded-lg border border-border-primary p-3 text-sm">
                    {plan.policy.reasons.length === 0 ? (
                      <p className="text-text-secondary">
                        {intl.formatMessage(messages.noneDeclared)}
                      </p>
                    ) : null}
                    {plan.policy.reasons.map((reason) => (
                      <p key={reason.code} className="break-words text-text-secondary">
                        {reason.message}
                      </p>
                    ))}
                  </div>
                </div>
              </section>
            </div>

            <DialogFooter className="sticky bottom-0 shrink-0 border-t border-border-primary bg-background-primary pt-4">
              <Button
                variant="outline"
                disabled={!!submitting}
                onClick={() => void decide('reject')}
              >
                {intl.formatMessage(
                  submitting === 'reject' ? messages.rejecting : messages.rejectPlan
                )}
              </Button>
              <Button
                variant={operation === 'uninstall' ? 'destructive' : 'default'}
                disabled={!!submitting || plan.policy.outcome === 'deny'}
                onClick={() => void decide('confirm')}
              >
                {intl.formatMessage(
                  submitting === 'confirm'
                    ? messages.confirming
                    : operation === 'uninstall'
                      ? messages.confirmUninstall
                      : messages.confirmStart
                )}
              </Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

function immutableEvidenceItems(
  plan: McpPlanReview,
  intl: IntlShape
): Array<[string, string | number]> {
  const evidence = plan.immutableEvidence;
  const items: Array<[string, string | number]> = [
    [intl.formatMessage(messages.evidenceType), formatMcpValue(intl, evidence.type)],
  ];
  if (evidence.type === 'artifact') {
    items.push([intl.formatMessage(messages.sha256), evidence.sha256]);
    if (evidence.size_bytes !== undefined) {
      items.push([intl.formatMessage(messages.sizeBytes), evidence.size_bytes]);
    }
  } else if (evidence.type === 'docker') {
    items.push(
      [intl.formatMessage(messages.image), evidence.image],
      [intl.formatMessage(messages.imageDigest), evidence.image_digest]
    );
  } else if (evidence.type === 'git_dev') {
    items.push(
      [intl.formatMessage(messages.repositoryOrigin), evidence.repository_origin],
      [intl.formatMessage(messages.commit), evidence.commit],
      [intl.formatMessage(messages.tree), evidenceValue(evidence.tree, intl)],
      [
        intl.formatMessage(messages.materializedDigest),
        evidenceValue(evidence.materialized_digest, intl),
      ]
    );
  } else {
    items.push([
      intl.formatMessage(messages.unavailableReason),
      formatMcpValue(intl, evidence.reason),
    ]);
  }
  return items;
}

function evidenceValue(
  evidence: Extract<McpPlanReview['immutableEvidence'], { type: 'git_dev' }>['tree'],
  intl: IntlShape
): string {
  return evidence.status === 'verified' ? evidence.value : formatMcpValue(intl, evidence.reason);
}
