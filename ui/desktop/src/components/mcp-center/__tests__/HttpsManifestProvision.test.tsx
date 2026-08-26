import { IntlProvider } from 'react-intl';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type {
  McpHttpsManifestPrepareResult,
  McpHttpsManifestConfirmResult,
  McpHttpsProvisionPlanReview,
} from '../../../acp/mcp-platform';
import type { McpTaskRef } from '@hikerm/lumina-sdk';
import { HttpsManifestProvision } from '../HttpsManifestProvision';
import {
  confirmHttpsMcpManifest,
  confirmMcpPlan,
  createHttpsProvisionPlanReview,
  prepareHttpsMcpManifest,
} from '../../../acp/mcp-platform';

vi.mock('../../../acp/mcp-platform', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../../acp/mcp-platform')>();
  return {
    ...actual,
    prepareHttpsMcpManifest: vi.fn(),
    confirmHttpsMcpManifest: vi.fn(),
    createHttpsProvisionPlanReview: vi.fn(),
    confirmMcpPlan: vi.fn(),
  };
});

const token = 'confirmation-token-sentinel';
const url = 'https://example.test/manifest.json?secret=query-sentinel';
const digest = 'sha256:digest-sentinel';
const task: McpTaskRef = {
  taskId: 'task-https-1',
  operation: 'install',
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

const prepared: McpHttpsManifestPrepareResult = {
  provisionId: 'provision-1',
  confirmationToken: token,
  expiresAtMs: Date.now() + 60_000,
  preview: {
    manifestId: 'safe-manifest-id',
    version: '1.0.0',
    redactedOrigin: 'https://example.test',
    rawDigest: digest,
    parsedDigest: 'parsed-sentinel',
    redirectChainDigest: 'redirect-sentinel',
    dnsEvidenceDigest: 'dns-sentinel',
    warnings: ['safe warning'],
  },
};

const plan: McpHttpsProvisionPlanReview = {
  planId: 'https-plan-1',
  planDigest: 'plan-digest-sentinel',
  expiresAtMs: Date.now() + 60_000,
  operation: 'provision',
  mcp: { mcpId: 'example', name: 'Example MCP', version: '1.0.0' },
  permissions: [{ kind: 'network', required: true }],
  effects: {
    file: { writesFiles: true, removesFiles: false, ownedItems: 2 },
    process: { processRequiredForConnection: true, startsDuringConfirmation: false },
  },
  confirmation: { required: ['policy'], defaultDisabled: true },
  policy: { outcome: 'allow', reasonCount: 0 },
  warnings: ['allowlist warning'],
};

function renderProvision() {
  const onTaskCreated = vi.fn();
  render(
    <IntlProvider locale="en">
      <HttpsManifestProvision onTaskCreated={onTaskCreated} />
    </IntlProvider>
  );
  return onTaskCreated;
}

async function openAndEnter(value = url, shouldRender = true) {
  if (shouldRender) {
    renderProvision();
  }
  const user = userEvent.setup();
  await user.click(screen.getByRole('button', { name: 'Import HTTPS manifest' }));
  const input = screen.getByRole('textbox');
  if (value.length > 0) {
    if (value.includes('::')) {
      fireEvent.change(input, { target: { value } });
    } else {
      await user.type(input, value);
    }
  }
  return user;
}

describe('HttpsManifestProvision', () => {
  beforeEach(() => {
    cleanup();
    vi.stubGlobal('crypto', { randomUUID: vi.fn(() => 'https-plan-key-1') });
    vi.mocked(prepareHttpsMcpManifest).mockReset();
    vi.mocked(confirmHttpsMcpManifest).mockReset();
    vi.mocked(createHttpsProvisionPlanReview).mockReset();
    vi.mocked(confirmMcpPlan).mockReset();
  });

  it.each([
    '',
    '   ',
    'http://example.test/manifest',
    'https://localhost/manifest',
    'https://api.localhost/manifest',
    'https://LOCAL/manifest',
    'https://service.example.local/manifest',
    'https://127.0.0.1/manifest',
    'https://127.255.255.255/manifest',
    'https://[::1]/manifest',
    'https://user:password@example.test/manifest',
  ])('rejects unsafe URL %j locally without prepare', async (value) => {
    const user = await openAndEnter(value);
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    expect(prepareHttpsMcpManifest).not.toHaveBeenCalled();
    expect(screen.getAllByRole('alert')).toHaveLength(1);
  });

  it.each([' https://example.test/manifest', 'https://example.test/manifest ', '\thttps://example.test/manifest', 'https://example.test/manifest\t'])('rejects raw surrounding whitespace %j without prepare', async (value) => {
    const user = await openAndEnter(value);
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    expect(prepareHttpsMcpManifest).not.toHaveBeenCalled();
    expect(screen.getAllByRole('alert')).toHaveLength(1);
  });

  it('preserves encoded hashes in the path when preparing', async () => {
    const encodedPathUrl = 'https://example.test/path%23segment/manifest.json';
    vi.mocked(prepareHttpsMcpManifest).mockResolvedValue(prepared);
    const user = await openAndEnter(encodedPathUrl);
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    expect(await screen.findByRole('status', { name: 'HTTPS manifest safety preview' })).toBeInTheDocument();
    expect(prepareHttpsMcpManifest).toHaveBeenCalledWith(encodedPathUrl);
  });

  it('shows only safe preview data and accessible validation wiring', async () => {
    vi.mocked(prepareHttpsMcpManifest).mockResolvedValue(prepared);
    const user = await openAndEnter();
    const input = screen.getByRole('textbox');
    expect(input).toHaveAttribute('aria-describedby', 'https-manifest-help');
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    expect(await screen.findByRole('status', { name: 'HTTPS manifest safety preview' })).toHaveTextContent('safe-manifest-id');
    const body = document.body.textContent ?? '';
    for (const hidden of [
      token,
      'query-sentinel',
      digest,
      'parsed-sentinel',
      'redirect-sentinel',
      'dns-sentinel',
    ]) {
      expect(body).not.toContain(hidden);
    }
    expect(screen.getByRole('status', { name: 'HTTPS manifest safety preview' })).toHaveAttribute('aria-label', 'HTTPS manifest safety preview');
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(screen.getByText('safe warning')).toBeInTheDocument();
    expect(screen.getByText('https://example.test')).toBeInTheDocument();
    expect(screen.getByText('safe warning').closest('div')).toBeTruthy();
    expect(input.closest('label')).toContainElement(input);
  });

  it('allows query strings but redacts them from preview and errors', async () => {
    vi.mocked(prepareHttpsMcpManifest).mockRejectedValue(new Error(`failed ${url}`));
    const user = await openAndEnter();
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    const alert = await screen.findByText('The HTTPS manifest could not be imported.');
    expect(prepareHttpsMcpManifest).toHaveBeenCalledWith(url);
    expect(alert.textContent).not.toContain('query-sentinel');
  });

  it('rejects fragments locally without leaking the URL or preparing', async () => {
    const fragmentUrl = 'https://example.test/manifest.json?secret=query-sentinel#fragment-token';
    const user = await openAndEnter(fragmentUrl);
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    expect(prepareHttpsMcpManifest).not.toHaveBeenCalled();
    expect(screen.getAllByRole('alert')).toHaveLength(1);
    expect(document.body.textContent).not.toContain('query-sentinel');
    expect(document.body.textContent).not.toContain('fragment-token');
  });

  it('confirms with the server token, then creates and safely confirms the https plan', async () => {
    vi.mocked(prepareHttpsMcpManifest).mockResolvedValue(prepared);
    vi.mocked(confirmHttpsMcpManifest).mockResolvedValue({
      manifestDigest: digest,
      preview: prepared.preview,
    });
    vi.mocked(createHttpsProvisionPlanReview).mockResolvedValue(plan);
    vi.mocked(confirmMcpPlan).mockResolvedValue(task);
    const onTaskCreated = renderProvision();
    const user = await openAndEnter(url, false);
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    await screen.findByRole('status', { name: 'HTTPS manifest safety preview' });
    await user.click(await screen.findByRole('button', { name: 'Continue' }));
    expect(prepareHttpsMcpManifest).toHaveBeenCalledWith(url);
    expect(confirmHttpsMcpManifest).toHaveBeenCalledWith({
      provisionId: 'provision-1',
      confirmationToken: token,
      confirm: true,
    });
    expect(createHttpsProvisionPlanReview).toHaveBeenCalledWith({
      provisionId: 'provision-1',
      expectedManifestDigest: digest,
      idempotencyKey: 'https-plan-key-1',
    });
    const calls = [
      vi.mocked(prepareHttpsMcpManifest).mock.invocationCallOrder[0],
      vi.mocked(confirmHttpsMcpManifest).mock.invocationCallOrder[0],
      vi.mocked(createHttpsProvisionPlanReview).mock.invocationCallOrder[0],
    ];
    expect(calls[0]).toBeLessThan(calls[1]);
    expect(calls[1]).toBeLessThan(calls[2]);
    const request = vi.mocked(createHttpsProvisionPlanReview).mock.calls[0][0];
    expect(request.idempotencyKey).not.toMatch(/token|https?:|query|digest|sentinel/i);
    expect(screen.getByText('allowlist warning')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Confirm and start' }));
    expect(confirmMcpPlan).toHaveBeenCalledWith(plan, 'confirm');
    expect(vi.mocked(confirmMcpPlan).mock.invocationCallOrder[0]).toBeGreaterThan(
      vi.mocked(createHttpsProvisionPlanReview).mock.invocationCallOrder[0]
    );
    await waitFor(() => expect(onTaskCreated).toHaveBeenCalledTimes(1));
    expect(onTaskCreated).toHaveBeenCalledWith(task);
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });

  it('does not duplicate preview or downstream requests on repeated clicks', async () => {
    let resolve: (value: McpHttpsManifestPrepareResult) => void = () => undefined;
    vi.mocked(prepareHttpsMcpManifest).mockImplementation(
      () =>
        new Promise((r) => {
          resolve = r;
        })
    );
    const user = await openAndEnter();
    const previewButton = screen.getByRole('button', { name: 'Preview' });
    await user.click(previewButton);
    await user.click(previewButton);
    resolve(prepared);
    await waitFor(() => expect(screen.getByRole('status', { name: 'HTTPS manifest safety preview' })).toBeInTheDocument());
    expect(prepareHttpsMcpManifest).toHaveBeenCalledTimes(1);
  });

  it('discards a stale confirm result after Escape and unmount', async () => {
    vi.mocked(prepareHttpsMcpManifest).mockResolvedValue(prepared);
    let resolve: (value: McpHttpsManifestConfirmResult) => void = () => undefined;
    vi.mocked(confirmHttpsMcpManifest).mockImplementation(
      () =>
        new Promise((r) => {
          resolve = r;
        })
    );
    const user = await openAndEnter();
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    await user.click(await screen.findByRole('button', { name: 'Continue' }));
    await user.keyboard('{Escape}');
    cleanup();
    resolve({ manifestDigest: digest, preview: prepared.preview });
    await act(async () => {
      await Promise.resolve();
    });
    expect(createHttpsProvisionPlanReview).not.toHaveBeenCalled();
    expect(confirmMcpPlan).not.toHaveBeenCalled();
  });

  it('allows only one confirm while confirming and keeps the dialog open until it resolves', async () => {
    vi.mocked(prepareHttpsMcpManifest).mockResolvedValue(prepared);
    let resolve: (value: McpHttpsManifestConfirmResult) => void = () => undefined;
    vi.mocked(confirmHttpsMcpManifest).mockImplementation(
      () =>
        new Promise((r) => {
          resolve = r;
        })
    );
    const user = await openAndEnter();
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    await screen.findByRole('status', { name: 'HTTPS manifest safety preview' });
    const continueButton = await screen.findByRole('button', { name: 'Continue' });
    await user.click(continueButton);

    expect(continueButton).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeDisabled();
    await user.click(continueButton);
    await user.keyboard('{Escape}');
    expect(confirmHttpsMcpManifest).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    expect(createHttpsProvisionPlanReview).not.toHaveBeenCalled();

    resolve({ manifestDigest: digest, preview: prepared.preview });
    await waitFor(() => expect(createHttpsProvisionPlanReview).toHaveBeenCalledTimes(1));
  });

  it('cancels preview without starting downstream work and restores entry focus', async () => {
    vi.mocked(prepareHttpsMcpManifest).mockResolvedValue(prepared);
    renderProvision();
    const entry = screen.getByRole('button', { name: 'Import HTTPS manifest' });
    const user = userEvent.setup();
    await user.click(entry);
    await user.type(screen.getByRole('textbox'), url);
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    await user.click(await screen.findByRole('button', { name: 'Cancel' }));
    expect(confirmHttpsMcpManifest).not.toHaveBeenCalled();
    expect(createHttpsProvisionPlanReview).not.toHaveBeenCalled();
    expect(entry).toHaveFocus();
  });

  it('closes plan review on Escape without starting the plan', async () => {
    vi.mocked(prepareHttpsMcpManifest).mockResolvedValue(prepared);
    vi.mocked(confirmHttpsMcpManifest).mockResolvedValue({
      manifestDigest: digest,
      preview: prepared.preview,
    });
    vi.mocked(createHttpsProvisionPlanReview).mockResolvedValue(plan);
    renderProvision();
    const entry = screen.getByRole('button', { name: 'Import HTTPS manifest' });
    const user = userEvent.setup();
    await user.click(entry);
    const reopenedInput = screen.getByRole('textbox');
    await user.clear(reopenedInput);
    await user.type(reopenedInput, url);
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    await user.click(await screen.findByRole('button', { name: 'Continue' }));
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Confirm and start' })).toBeInTheDocument()
    );
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('button', { name: 'Confirm and start' })).not.toBeInTheDocument();
    expect(confirmMcpPlan).not.toHaveBeenCalled();
  });

  it('does not update the DOM or issue downstream requests after unmounting pending work', async () => {
    let resolvePrepare: (value: McpHttpsManifestPrepareResult) => void = () => undefined;
    vi.mocked(prepareHttpsMcpManifest).mockImplementation(
      () =>
        new Promise((r) => {
          resolvePrepare = r;
        })
    );
    const user = await openAndEnter();
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    cleanup();
    resolvePrepare(prepared);
    await act(async () => {
      await Promise.resolve();
    });
    expect(createHttpsProvisionPlanReview).not.toHaveBeenCalled();
    expect(document.body.textContent).not.toContain('safe-manifest-id');
  });

  it('keeps the dialog open during preparing and shows the resolved preview after Escape or close attempts', async () => {
    let resolve: (value: McpHttpsManifestPrepareResult) => void = () => undefined;
    vi.mocked(prepareHttpsMcpManifest).mockImplementation(
      () =>
        new Promise((r) => {
          resolve = r;
        })
    );
    const user = await openAndEnter();
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeDisabled();
    await user.keyboard('{Escape}');
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    resolve(prepared);
    expect(await screen.findByRole('status', { name: 'HTTPS manifest safety preview' })).toHaveTextContent('safe-manifest-id');
    expect(confirmHttpsMcpManifest).not.toHaveBeenCalled();
  });

  it('renders safe fallback errors without leaking internal values and clears token after failure', async () => {
    vi.mocked(prepareHttpsMcpManifest).mockRejectedValue(
      new Error(`failed ${token} ${url} internal-proof`)
    );
    const user = await openAndEnter();
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    const text =
      (await screen.findByText('The HTTPS manifest could not be imported.')).textContent ?? '';
    expect(await screen.findAllByRole('alert')).toHaveLength(1);
    expect(text).not.toContain(token);
    expect(text).not.toContain('query-sentinel');
    expect(text).not.toContain('internal-proof');
  });

  it('keeps prepare failures safe and retryable', async () => {
    vi.mocked(prepareHttpsMcpManifest)
      .mockRejectedValueOnce(new Error(`raw ${token} ${url} body-proof`))
      .mockResolvedValueOnce(prepared);
    const user = await openAndEnter();
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    const alert = await screen.findByText('The HTTPS manifest could not be imported.');
    expect(await screen.findAllByRole('alert')).toHaveLength(1);
    expect(alert.textContent).not.toMatch(/token|https?:|body-proof|query-sentinel/i);
    expect(screen.getByRole('button', { name: 'Retry' })).toBeEnabled();
    await user.click(screen.getByRole('button', { name: 'Retry' }));
    expect(await screen.findByRole('status', { name: 'HTTPS manifest safety preview' })).toBeInTheDocument();
    expect(prepareHttpsMcpManifest).toHaveBeenCalledTimes(2);
  });

  it('clears confirmation capability when confirm fails and does not create a plan', async () => {
    vi.mocked(prepareHttpsMcpManifest).mockResolvedValue(prepared);
    vi.mocked(confirmHttpsMcpManifest).mockRejectedValue(new Error(`failed ${token} ${url}`));
    const user = await openAndEnter();
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    await user.click(await screen.findByRole('button', { name: 'Continue' }));
    expect(await screen.findAllByRole('alert')).toHaveLength(1);
    expect(createHttpsProvisionPlanReview).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    expect(confirmHttpsMcpManifest).toHaveBeenCalledTimes(1);
    expect(createHttpsProvisionPlanReview).not.toHaveBeenCalled();
  });

  it('does not confirm when plan creation fails or policy denies the plan', async () => {
    vi.mocked(prepareHttpsMcpManifest).mockResolvedValue(prepared);
    vi.mocked(confirmHttpsMcpManifest).mockResolvedValue({
      manifestDigest: digest,
      preview: prepared.preview,
    });
    vi.mocked(createHttpsProvisionPlanReview).mockRejectedValueOnce(new Error('plan rejected'));
    const user = await openAndEnter();
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    await user.click(await screen.findByRole('button', { name: 'Continue' }));
    expect(await screen.findAllByRole('alert')).toHaveLength(1);
    expect(confirmMcpPlan).not.toHaveBeenCalled();

    vi.mocked(createHttpsProvisionPlanReview).mockResolvedValue({
      ...plan,
      policy: { outcome: 'deny', reasonCount: 1 },
    });
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    await user.click(await screen.findByRole('button', { name: 'Continue' }));
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Confirm and start' })).toBeDisabled()
    );
    expect(confirmMcpPlan).not.toHaveBeenCalled();
  });

  it('deduplicates every request in the successful chain', async () => {
    vi.mocked(prepareHttpsMcpManifest).mockResolvedValue(prepared);
    vi.mocked(confirmHttpsMcpManifest).mockResolvedValue({
      manifestDigest: digest,
      preview: prepared.preview,
    });
    vi.mocked(createHttpsProvisionPlanReview).mockResolvedValue(plan);
    let resolveConfirm: (value: McpTaskRef) => void = () => undefined;
    vi.mocked(confirmMcpPlan).mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveConfirm = resolve;
        })
    );
    const onTaskCreated = renderProvision();
    const user = await openAndEnter(url, false);
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    await user.click(await screen.findByRole('button', { name: 'Continue' }));
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Confirm and start' })).toBeInTheDocument()
    );
    const confirmButton = screen.getByRole('button', { name: 'Confirm and start' });
    await user.click(confirmButton);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Confirming…' })).toBeDisabled());
    expect(confirmButton).toBeDisabled();
    await user.click(confirmButton);
    await waitFor(() => expect(confirmMcpPlan).toHaveBeenCalledTimes(1));
    resolveConfirm(task);
    await waitFor(() => expect(onTaskCreated).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(prepareHttpsMcpManifest).toHaveBeenCalledTimes(1);
    expect(confirmHttpsMcpManifest).toHaveBeenCalledTimes(1);
    expect(createHttpsProvisionPlanReview).toHaveBeenCalledTimes(1);
    expect(confirmMcpPlan).toHaveBeenCalledTimes(1);
  });

  it('keeps the plan review safe and retryable when task creation fails', async () => {
    vi.mocked(prepareHttpsMcpManifest).mockResolvedValue(prepared);
    vi.mocked(confirmHttpsMcpManifest).mockResolvedValue({
      manifestDigest: digest,
      preview: prepared.preview,
    });
    vi.mocked(createHttpsProvisionPlanReview).mockResolvedValue(plan);
    vi.mocked(confirmMcpPlan)
      .mockRejectedValueOnce(new Error(`task failed ${token} ${url} internal-proof`))
      .mockResolvedValueOnce(task);
    const onTaskCreated = renderProvision();
    const user = await openAndEnter(url, false);
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    await user.click(await screen.findByRole('button', { name: 'Continue' }));
    await user.click(await screen.findByRole('button', { name: 'Confirm and start' }));

    const alert = await screen.findByText('The MCP Platform request could not be completed.');
    expect(screen.getAllByRole('alert')).toHaveLength(1);
    expect(alert).toHaveTextContent('The MCP Platform request could not be completed.');
    expect(alert.textContent).not.toMatch(/token|https?:|query-sentinel|internal-proof/i);
    expect(onTaskCreated).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Confirm and start' })).toBeEnabled();
    expect(screen.getByRole('dialog')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Confirm and start' }));
    await waitFor(() => expect(onTaskCreated).toHaveBeenCalledWith(task));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(onTaskCreated).toHaveBeenCalledTimes(1);
  });

  it('hands the created task to the caller without claiming monitor ownership', async () => {
    vi.mocked(prepareHttpsMcpManifest).mockResolvedValue(prepared);
    vi.mocked(confirmHttpsMcpManifest).mockResolvedValue({
      manifestDigest: digest,
      preview: prepared.preview,
    });
    vi.mocked(createHttpsProvisionPlanReview).mockResolvedValue(plan);
    vi.mocked(confirmMcpPlan).mockResolvedValue(task);
    const onTaskCreated = renderProvision();
    const user = await openAndEnter(url, false);
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    await user.click(await screen.findByRole('button', { name: 'Continue' }));
    await user.click(await screen.findByRole('button', { name: 'Confirm and start' }));
    await waitFor(() => expect(onTaskCreated).toHaveBeenCalledWith(task));
    expect(onTaskCreated).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });
});
