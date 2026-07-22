import { IntlProvider } from 'react-intl';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { McpPlanReview, McpTaskRef } from '@aaif/goose-sdk';
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

describe('PlanReviewDialog', () => {
  beforeEach(() => vi.mocked(confirmMcpPlan).mockReset());

  it('blocks confirmation when policy denies the plan', () => {
    renderDialog(plan('deny'));

    expect(screen.getByRole('button', { name: 'Confirm and start' })).toBeDisabled();
  });

  it('renders immutable evidence, network origins, registrations, and all confirmation details', () => {
    renderDialog(plan('allow'), 'update');

    expect(screen.getByText('Update')).toBeInTheDocument();
    expect(screen.getByText('sha256:abc')).toBeInTheDocument();
    expect(screen.getByText('https://api.example.test')).toBeInTheDocument();
    expect(screen.getByText('https://cdn.example.test')).toBeInTheDocument();
    expect(screen.getByText('registration-a')).toBeInTheDocument();
    expect(screen.getByText('registration-b')).toBeInTheDocument();
    expect(screen.getByText(/Reason code: network_review/)).toBeInTheDocument();
    expect(screen.getByText(/Permission ID: network-access/)).toBeInTheDocument();
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
});
