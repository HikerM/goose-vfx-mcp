import { useState } from 'react';
import type { IntlShape } from 'react-intl';
import type { McpPlanReview, McpTaskRef } from '@aaif/goose-sdk';
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
}: {
  plan: McpPlanReview | null;
  operation: McpTaskRef['operation'] | null;
  onClose: () => void;
  onTaskCreated: (task: McpTaskRef) => void;
}) {
  const intl = useIntl();
  const [submitting, setSubmitting] = useState<'confirm' | 'reject' | null>(null);
  const [error, setError] = useState<McpPlatformRecoveryViewModel | null>(null);

  const decide = async (decision: 'confirm' | 'reject') => {
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
    <Dialog open={!!plan} onOpenChange={(open) => !open && !submitting && onClose()}>
      <DialogContent className="max-h-[85vh] max-w-3xl overflow-y-auto">
        {plan && (
          <>
            <DialogHeader>
              <DialogTitle>{intl.formatMessage(messages.reviewPlan)}</DialogTitle>
              <DialogDescription>
                {intl.formatMessage(messages.reviewPlanDescription)}
              </DialogDescription>
            </DialogHeader>

            {error && <RecoveryPanel recovery={error} />}

            <section className="space-y-4" aria-label={intl.formatMessage(messages.planImpact)}>
              <div className="flex flex-wrap items-center gap-2">
                <h3 className="text-lg font-medium text-text-primary">{plan.name}</h3>
                <StatusBadge>{plan.version}</StatusBadge>
                <StatusBadge tone={plan.policy.outcome === 'deny' ? 'danger' : 'info'}>
                  {intl.formatMessage(messages.policy)}: {formatMcpValue(intl, plan.policy.outcome)}
                </StatusBadge>
              </div>
              <DefinitionList
                items={[
                  [intl.formatMessage(messages.operation), formatMcpValue(intl, operation ?? '')],
                  [intl.formatMessage(messages.source), plan.sourceId],
                  [intl.formatMessage(messages.scope), intl.formatMessage(messages.currentUser)],
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
              <div>
                <h3 className="mb-2 font-medium text-text-primary">
                  {intl.formatMessage(messages.immutableEvidence)}
                </h3>
                <DefinitionList items={immutableEvidenceItems(plan, intl)} />
              </div>
              <div>
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
              <div>
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
              <div>
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
                        <div className="mt-1 text-text-secondary">{permission.reason}</div>
                        {permission.scope && (
                          <div className="mt-1 font-mono text-xs">{permission.scope}</div>
                        )}
                      </li>
                    ))}
                  </ul>
                )}
              </div>
              <div>
                <h3 className="mb-2 font-medium text-text-primary">
                  {intl.formatMessage(messages.policyRollback)}
                </h3>
                <div className="space-y-2 rounded-lg border border-border-primary p-3 text-sm">
                  <p className="text-text-primary">
                    {intl.formatMessage(messages.recovery)}:{' '}
                    {formatMcpValue(intl, plan.reversibility.strategy.type)}
                  </p>
                  {plan.policy.reasons.map((reason) => (
                    <p key={reason.code} className="text-text-secondary">
                      {reason.message}
                    </p>
                  ))}
                  {plan.warnings.map((warning) => (
                    <p key={warning} className="text-text-warning">
                      {formatMcpValue(intl, warning)}
                    </p>
                  ))}
                </div>
              </div>
            </section>

            <DialogFooter>
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
          </>
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
