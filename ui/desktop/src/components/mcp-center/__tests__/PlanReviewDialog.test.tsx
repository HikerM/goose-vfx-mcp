import { IntlProvider } from 'react-intl';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { McpPlanReview, McpTaskRef } from '@aaif/goose-sdk';
import type { McpHttpsProvisionPlanReview } from '../../../acp/mcp-platform';
import { PlanReviewDialog } from '../PlanReviewDialog';
import { confirmMcpPlan } from '../../../acp/mcp-platform';

vi.mock('../../../acp/mcp-platform', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../../acp/mcp-platform')>();
  return { ...actual, confirmMcpPlan: vi.fn() };
});

const plan = (outcome: McpPlanReview['policy']['outcome']): McpPlanReview => ({
  planId: 'plan-1',
  planDigest: 'digest-1',
  expiresAtMs: Date.now() + 60_000,
  sourceId: 'catalog',
  catalogTarget: {
    sourceId: 'catalog',
    mcpId: 'example',
    version: '1.0.0',
    manifestDigest: 'manifest-1',
  },
  proof: { type: 'local_bytes' },
  trustTier: 'official',
  publisher: { id: 'publisher', name: 'Publisher', signingIdentities: [] },
  mcpId: 'example',
  name: 'Example MCP',
  version: '1.0.0',
  selectedManifestDigest: 'manifest-1',
  immutableEvidence: { type: 'artifact', sha256: 'sha256:abc', size_bytes: 42 },
  permissions: [],
  networkOrigins: ['https://api.example.test', 'https://cdn.example.test'],
  fileEffects: { writesFiles: false, removesFiles: false, ownedItems: 0 },
  hostEffects: { registrationIds: ['registration-a', 'registration-b'] },
  processEffects: { processRequiredForConnection: false, startsDuringConfirmation: false },
  reversibility: { reversible: true, strategy: { type: 'remove_connection_registration' } },
  policy: { outcome, reasons: [] },
  warnings: [],
  requiredConfirmations: [
    { type: 'policy', reason_code: 'network_review' },
    { type: 'permission', permission_id: 'network-access' },
  ],
  defaultDisabled: true,
  recovery: 'none',
});

const task: McpTaskRef = {
  taskId: 'task-1',
  operation: 'register',
  status: 'queued',
  progress: 0,
  cancellable: true,
  revision: 1,
  updatedAtMs: Date.now(),
  outcome: {
    state: 'pending',
    rollback: 'not_required',
    finalization: 'pending',
    remainingEffects: [],
    nextAction: 'wait',
  },
};

const httpsPlan: McpHttpsProvisionPlanReview = {
  planId: 'https-plan-safe',
  planDigest: 'https-plan-digest-secret',
  expiresAtMs: Date.now() + 60_000,
  operation: 'provision',
  mcp: { mcpId: 'https-mcp', name: 'HTTPS MCP', version: '2.0.0' },
  permissions: [{ kind: 'network', required: true }],
  effects: {
    file: { writesFiles: true, removesFiles: false, ownedItems: 1 },
    process: { processRequiredForConnection: true, startsDuringConfirmation: false },
  },
  confirmation: { required: ['policy'], defaultDisabled: true },
  policy: { outcome: 'allow', reasonCount: 0 },
  warnings: ['allowlist-safe-fixture'],
};

const deniedHttpsPlan = { ...httpsPlan, policy: { outcome: 'deny' as const, reasonCount: 1 } };

function renderDialog(review: McpPlanReview, operation: McpTaskRef['operation'] = 'install') {
  const onClose = vi.fn();
  const onTaskCreated = vi.fn();
  render(
    <IntlProvider locale="en">
      <PlanReviewDialog
        plan={review}
        operation={operation}
        onClose={onClose}
        onTaskCreated={onTaskCreated}
      />
    </IntlProvider>
  );
  return { onClose, onTaskCreated };
}

function renderHttpsDialog() {
  const onClose = vi.fn();
  const onTaskCreated = vi.fn();
  render(
    <IntlProvider locale="en">
      <PlanReviewDialog
        plan={null}
        httpsPlan={httpsPlan}
        operation="provision"
        onClose={onClose}
        onTaskCreated={onTaskCreated}
      />
    </IntlProvider>
  );
  return { onClose, onTaskCreated };
}

function renderHttpsPlanDialog(review: McpHttpsProvisionPlanReview) {
  const onClose = vi.fn();
  const onTaskCreated = vi.fn();
  render(
    <IntlProvider locale="en">
      <PlanReviewDialog
        plan={null}
        httpsPlan={review}
        operation="provision"
        onClose={onClose}
        onTaskCreated={onTaskCreated}
      />
    </IntlProvider>
  );
  return { onClose, onTaskCreated };
}

describe('PlanReviewDialog', () => {
  beforeEach(() => {
    cleanup();
    vi.mocked(confirmMcpPlan).mockReset();
  });

  it('blocks confirmation when policy denies the plan', () => {
    renderDialog(plan('deny'));

    expect(screen.getByRole('button', { name: 'Confirm and start' })).toBeDisabled();
  });

  it('renders immutable evidence, network origins, registrations, and all confirmation details', () => {
    renderDialog(plan('allow'), 'update');

    expect(screen.getByText('Update')).toBeInTheDocument();
    expect(screen.getByText('Plan expires')).toBeInTheDocument();
    expect(screen.getByText('Catalog item')).toBeInTheDocument();
    expect(screen.getByText('catalog / example / 1.0.0')).toBeInTheDocument();
    expect(screen.getByText('Manifest digest')).toBeInTheDocument();
    expect(screen.getByText('manifest-1')).toBeInTheDocument();
    expect(screen.getByText('sha256:abc')).toBeInTheDocument();
    expect(screen.getByText('https://api.example.test')).toBeInTheDocument();
    expect(screen.getByText('https://cdn.example.test')).toBeInTheDocument();
    expect(screen.getByText('registration-a')).toBeInTheDocument();
    expect(screen.getByText('registration-b')).toBeInTheDocument();
    expect(screen.getByText(/Reason code: network_review/)).toBeInTheDocument();
    expect(screen.getByText(/Permission ID: network-access/)).toBeInTheDocument();
    expect(
      screen.getByText(/does not expose executor steps, shell commands, credentials/i)
    ).toBeInTheDocument();
    expect(screen.getByText('Warnings')).toBeInTheDocument();
    expect(screen.getByText('No plan warnings.')).toBeInTheDocument();
  });

  it.each(['install', 'update', 'repair'] as const)(
    'confirms the %s review path',
    async (operation) => {
      vi.mocked(confirmMcpPlan).mockResolvedValue(task);
      const user = userEvent.setup();
      renderDialog(plan('allow'), operation);

      await user.click(screen.getByRole('button', { name: 'Confirm and start' }));

      expect(confirmMcpPlan).toHaveBeenCalledTimes(1);
    }
  );

  it('uses an explicit destructive confirmation for uninstall', async () => {
    vi.mocked(confirmMcpPlan).mockResolvedValue({ ...task, operation: 'uninstall' });
    const user = userEvent.setup();
    renderDialog(plan('allow'), 'uninstall');

    expect(screen.getByRole('alert')).toHaveTextContent(
      /removes the MCP registration and owned files/i
    );
    const confirm = screen.getByRole('button', { name: 'Confirm uninstall' });
    expect(confirm).toHaveClass('bg-background-danger');
    await user.click(confirm);

    expect(confirmMcpPlan).toHaveBeenCalledTimes(1);
  });

  it('confirms only after the user activates the explicit confirm button', async () => {
    vi.mocked(confirmMcpPlan).mockResolvedValue(task);
    const user = userEvent.setup();
    const { onClose, onTaskCreated } = renderDialog(plan('allow'));

    expect(confirmMcpPlan).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Confirm and start' }));

    expect(confirmMcpPlan).toHaveBeenCalledWith(
      expect.objectContaining({ planId: 'plan-1' }),
      'confirm'
    );
    expect(onTaskCreated).toHaveBeenCalledWith(task);
    expect(onClose).toHaveBeenCalled();
  });

  it('closes the dialog when Escape is pressed before submission starts', async () => {
    const user = userEvent.setup();
    const { onClose } = renderDialog(plan('allow'));

    await user.keyboard('{Escape}');

    expect(onClose).toHaveBeenCalled();
  });

  it('keeps HTTPS review limited to safe fields and confirms the typed plan', async () => {
    vi.mocked(confirmMcpPlan).mockResolvedValue(task);
    const { onClose, onTaskCreated } = renderHttpsDialog();
    expect(screen.getByText('allowlist-safe-fixture')).toBeInTheDocument();
    const body = document.body.textContent ?? '';
    for (const hidden of [
      'https-plan-digest-secret',
      'https://secret.example.test/path?query=private',
      'publisher-unique',
      'proof-unique',
      'internal-sentinel',
      'catalog',
      'remote_http',
      'manual',
    ]) {
      expect(body).not.toContain(hidden);
    }
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: 'Confirm and start' }));
    expect(confirmMcpPlan).toHaveBeenCalledWith(httpsPlan, 'confirm');
    expect(onTaskCreated).toHaveBeenCalledTimes(1);
    expect(onTaskCreated).toHaveBeenCalledWith(task);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('disables HTTPS confirmation for denied policy and accepts a safe plan with plan=null', () => {
    renderHttpsPlanDialog(deniedHttpsPlan);
    expect(screen.getByRole('button', { name: 'Confirm and start' })).toBeDisabled();
    expect(screen.getByText('HTTPS MCP')).toBeInTheDocument();
    expect(screen.getByText('allowlist-safe-fixture')).toBeInTheDocument();
  });

  it('rejects and escapes HTTPS review without downstream confirmation', async () => {
    const { onClose } = renderHttpsDialog();
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: 'Reject plan' }));
    expect(confirmMcpPlan).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledTimes(1);

    cleanup();
    const { onClose: escapeOnClose } = renderHttpsDialog();
    await user.keyboard('{Escape}');
    expect(confirmMcpPlan).not.toHaveBeenCalled();
    expect(escapeOnClose).toHaveBeenCalled();
  });

  it('shows a safe error and allows retry for HTTPS confirmation', async () => {
    vi.mocked(confirmMcpPlan)
      .mockRejectedValueOnce(
        new Error('internal-sentinel https://secret.example.test?token=private')
      )
      .mockResolvedValueOnce(task);
    const user = userEvent.setup();
    const { onTaskCreated } = renderHttpsDialog();
    await user.click(screen.getByRole('button', { name: 'Confirm and start' }));
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(/unable|failed|error|retry/i);
    expect(alert.textContent).not.toContain('internal-sentinel');
    expect(alert.textContent).not.toContain('secret.example.test');
    await user.click(screen.getByRole('button', { name: 'Confirm and start' }));
    expect(confirmMcpPlan).toHaveBeenCalledTimes(2);
    expect(onTaskCreated).toHaveBeenCalledTimes(1);
  });

  it('does not leak sensitive HTTPS values in DOM or retry error, and closes once after success', async () => {
    const freshToken = 'fresh-token-only-in-request';
    vi.mocked(confirmMcpPlan)
      .mockRejectedValueOnce(
        new Error(
          'token=old-token url=https://internal.test/?q=secret digest=old-digest provenance=private publisher=private proof=private internal-sentinel'
        )
      )
      .mockResolvedValueOnce(task);
    const user = userEvent.setup();
    const { onClose, onTaskCreated } = renderHttpsPlanDialog({
      ...httpsPlan,
      planDigest: freshToken,
    });
    await user.click(screen.getByRole('button', { name: 'Confirm and start' }));
    const alert = await screen.findByRole('alert');
    for (const value of [
      'old-token',
      'internal.test',
      'secret',
      'old-digest',
      'provenance=private',
      'publisher=private',
      'proof=private',
      'internal-sentinel',
    ]) {
      expect(alert).not.toHaveTextContent(value);
    }
    await user.click(screen.getByRole('button', { name: 'Confirm and start' }));
    await waitFor(() => expect(onTaskCreated).toHaveBeenCalledTimes(1));
    expect(confirmMcpPlan).toHaveBeenNthCalledWith(
      1,
      expect.objectContaining({ planDigest: freshToken }),
      'confirm'
    );
    expect(confirmMcpPlan).toHaveBeenNthCalledWith(
      2,
      expect.objectContaining({ planDigest: freshToken }),
      'confirm'
    );
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
