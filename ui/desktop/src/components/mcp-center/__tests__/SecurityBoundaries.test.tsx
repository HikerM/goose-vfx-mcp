import { IntlProvider } from 'react-intl';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type {
  McpHealthStatus,
  McpManagedDetail,
  McpManagedSummary,
  McpPlanReview,
  McpSourcesPolicyState,
  McpTaskRef,
} from '@aaif/goose-sdk';
import { ManualTab } from '../ManualTab';
import { ManagedTab } from '../ManagedTab';
import { SourcesPolicyTab } from '../SourcesPolicyTab';
import { CredentialAttentionPanel, RecoveryPanel } from '../McpCenterCommon';
import {
  confirmMcpSourceProvision,
  createManualMcpPlan,
  getManagedMcp,
  getMcpHealth,
  getMcpTask,
  importGovernedMcpSource,
  listManagedMcps,
  listManualStdioSources,
  mapMcpPlatformError,
  prepareMcpSourceProvision,
  refreshMcpSource,
  resumeMcpEvents,
  setMcpDefaultEnabled,
} from '../../../acp/mcp-platform';

vi.mock('../../../acp/mcp-platform', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../../acp/mcp-platform')>();
  return {
    ...actual,
    confirmMcpSourceProvision: vi.fn(),
    createManualMcpPlan: vi.fn(),
    getManagedMcp: vi.fn(),
    getMcpHealth: vi.fn(),
    getMcpTask: vi.fn(),
    importGovernedMcpSource: vi.fn(),
    listManagedMcps: vi.fn(),
    listManualStdioSources: vi.fn(),
    prepareMcpSourceProvision: vi.fn(),
    refreshMcpSource: vi.fn(),
    resumeMcpEvents: vi.fn(),
    setMcpDefaultEnabled: vi.fn(),
  };
});

const policy: McpSourcesPolicyState = {
  policy: {
    targetPlatform: 'windows',
    targetArchitecture: 'x86_64',
    developmentMode: false,
    dockerAllowed: false,
    recovery: 'none',
  },
  sources: [],
};

const refreshablePolicy: McpSourcesPolicyState = {
  policy: policy.policy,
  sources: [
    {
      sourceId: 'verified_source_catalog_source-alpha',
      displayName: 'Alpha Source',
      importKind: 'verified_source_catalog',
      trustTiers: ['community'],
      manifestCount: 2,
      lastImportedAtMs: 1,
      cache: {
        offline: false,
        localPersistenceOnly: false,
        newestVerifiedAtMs: 1,
        freshness: 'fresh',
        refreshState: 'idle',
        recovery: 'none',
      },
      compatibility: { compatible: 2, restricted: 0, denied: 0 },
      recovery: 'none',
      lastRefreshDocumentDigest: 'abcd',
      lastRefreshedAtMs: 1,
    },
  ],
};

function withIntl(node: React.ReactNode) {
  return render(<IntlProvider locale="en">{node}</IntlProvider>);
}

const task = (overrides: Partial<McpTaskRef> = {}): McpTaskRef => ({
  taskId: 'task-security',
  operation: 'update',
  status: 'failed',
  progress: 100,
  cancellable: false,
  revision: 2,
  updatedAtMs: 2,
  outcome: {
    state: 'failed',
    error: {
      code: 'adapter_failed',
      retryable: false,
      correlationId: 'task-security-ref',
      message:
        'Task failed. Authorization: Basic dXNlcjpwYXNz ' +
        'Proxy-Authorization: Negotiate super-secret tail-token nextStep=Retry ' +
        '{"Authorization":"Bearer second-secret","X-Api-Key":"abc123"} path=/tmp ' +
        'Failure while reading /tmp, then (ws://private.example.test/socket).',
      suggestion: 'retry',
    },
    rollback: 'not_required',
    finalization: 'complete',
    remainingEffects: [],
    nextAction: 'wait',
  },
  ...overrides,
});

const summary = (
  managedMcpId: string,
  revision: number,
  overrides: Partial<McpManagedSummary> = {}
): McpManagedSummary => ({
  managedMcpId,
  mcpId: 'Security MCP',
  installationScope: 'user',
  registration: 'registered',
  installation: 'installed',
  runtime: 'stopped',
  health: 'healthy',
  defaultEnabled: false,
  revision,
  updatedAtMs: revision,
  distributionAdapter: 'archive',
  activeVersion: '1.0.0',
  availableVersion: null,
  availableManifestDigest: null,
  currentTask: null,
  recoveryRequired: false,
  eligibility: {
    update: false,
    repair: false,
    uninstall: false,
    reason: 'eligible',
    updateReason: 'no_update_available',
    repairReason: 'eligible',
    uninstallReason: 'eligible',
  },
  nextAction: 'none',
  phaseCapabilities: {
    sessionEnablement: 'not_available_in_this_phase',
    toolPolicy: 'not_available_in_this_phase',
    profiles: 'not_available_in_this_phase',
    modelSuggestions: 'not_available_in_this_phase',
  },
  ...overrides,
});

const detail = (item: McpManagedSummary): McpManagedDetail => ({
  summary: item,
  distributionAdapter: item.distributionAdapter,
  activeManifestDigest: `manifest-${item.managedMcpId}`,
  activeVersion: item.activeVersion ?? '',
  extensionConfigKey: `extension-${item.managedMcpId}`,
  projectionDigest: `projection-${item.managedMcpId}`,
  latestHealth: null,
  registrationTask: null,
});

const health = (managedMcpId: string): McpHealthStatus => ({
  managedMcpId,
  state: 'healthy',
  latest: null,
});

const plan: McpPlanReview = {
  planId: 'plan-manual',
  planDigest: 'digest-manual',
  expiresAtMs: Date.now() + 60_000,
  sourceId: 'manual',
  proof: { type: 'local_bytes' },
  trustTier: 'local',
  publisher: { id: 'publisher', name: 'Publisher', signingIdentities: [] },
  mcpId: 'manual-http',
  name: 'Manual HTTP',
  version: '1.0.0',
  selectedManifestDigest: 'digest-manual-http',
  immutableEvidence: { type: 'artifact', sha256: 'sha256:test' },
  permissions: [],
  networkOrigins: [],
  fileEffects: { writesFiles: false, removesFiles: false, ownedItems: 0 },
  hostEffects: { registrationIds: [] },
  processEffects: { processRequiredForConnection: false, startsDuringConfirmation: false },
  reversibility: { reversible: true, strategy: { type: 'remove_connection_registration' } },
  policy: { outcome: 'allow', reasons: [] },
  warnings: [],
  requiredConfirmations: [],
  defaultDisabled: true,
  recovery: 'none',
};

describe('MCP Center security boundaries', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(listManualStdioSources).mockResolvedValue({
      provider: 'available',
      items: [
        {
          sourceId: 'approved-source',
          displayName: 'Approved source',
          publisherName: 'Publisher',
          trustTier: 'official',
          compatibility: 'compatible',
        },
      ],
    });
    vi.mocked(createManualMcpPlan).mockResolvedValue(plan);
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [], nextCursor: null });
    vi.mocked(getManagedMcp).mockImplementation(async (managedMcpId) =>
      detail(summary(String(managedMcpId), 1))
    );
    vi.mocked(getMcpHealth).mockImplementation(async (managedMcpId) =>
      health(String(managedMcpId))
    );
    vi.mocked(getMcpTask).mockImplementation(async (taskId) => task({ taskId: String(taskId) }));
    vi.mocked(importGovernedMcpSource).mockResolvedValue({
      source: {
        sourceId: 'local_manifest_source',
        importKind: 'local_persistence',
        displayName: 'Local manifest source',
      },
      document: {
        sourceId: 'local_manifest_source',
        documentId: 'manifest_deadbeef',
        documentDigest: 'deadbeef',
        documentKind: 'manifest',
      },
      entries: [
        {
          sourceId: 'local_manifest_source',
          mcpId: 'pkg.test',
          version: '1.0.0',
          manifestDigest: 'manifest-digest',
          name: 'Pkg test',
          description: 'Test import',
          publisherId: 'publisher',
          publisherName: 'Publisher',
          trustTier: 'local',
          proof: { type: 'local_bytes' },
          compatibility: 'compatible',
          distribution: 'remote_http',
          verifiedAtMs: 1,
          eligibility: { outcome: 'allowed', reason: 'eligible', recovery: 'none' },
        },
      ],
    });
    vi.mocked(prepareMcpSourceProvision).mockResolvedValue({
      provisionId: 'source_provisioning_alpha',
      confirmationToken: 'source_provisioning_confirmation_not_for-display',
      expiresAtMs: Date.now() + 60_000,
      preview: {
        sourceId: 'verified_source_catalog_source-alpha',
        displayName: 'Alpha Source',
        rootDigest: 'root-alpha',
        documentDigest: 'document-alpha',
        endpointHost: 'catalog.example.test',
        refreshTransport: 'verified_source_bundle_v1',
        manifestCount: 2,
        warnings: ['The source is pinned to this signed root.'],
        trustBasis: 'user_pin',
      },
    });
    vi.mocked(confirmMcpSourceProvision).mockResolvedValue({
      provisionId: 'source_provisioning_alpha',
      confirmedAtMs: 2,
      preview: {
        sourceId: 'verified_source_catalog_source-alpha',
        displayName: 'Alpha Source',
        rootDigest: 'root-alpha',
        documentDigest: 'document-alpha',
        endpointHost: 'catalog.example.test',
        refreshTransport: 'verified_source_bundle_v1',
        manifestCount: 2,
        warnings: ['The source is pinned to this signed root.'],
        trustBasis: 'user_pin',
      },
    });
    vi.mocked(refreshMcpSource).mockResolvedValue({
      sourceId: 'verified_source_catalog_source-alpha',
      displayName: 'Alpha Source',
      documentDigest: 'document-alpha',
      manifestCount: 2,
      refreshedAtMs: 2,
    });
    vi.mocked(resumeMcpEvents).mockResolvedValue({ events: [], tasks: [], nextEventId: 0 });
    vi.mocked(setMcpDefaultEnabled).mockImplementation(
      async (
        selected: Pick<McpManagedSummary, 'managedMcpId' | 'revision'>,
        enabled: boolean
      ): Promise<McpManagedSummary> => ({
        ...summary(selected.managedMcpId, selected.revision + 1),
        defaultEnabled: enabled,
      })
    );
    window.electron.selectFileOrDirectory = vi
      .fn()
      .mockResolvedValue('/tmp/catalogs/manifest.json');
    window.electron.directoryChooser = vi.fn().mockResolvedValue({
      canceled: false,
      filePaths: ['/tmp/catalogs/source-alpha'],
    });
  });

  it('does not render HTTP token, password, header, or credential-reference inputs', async () => {
    const user = userEvent.setup();
    withIntl(<ManualTab onTaskCreated={vi.fn()} />);
    await screen.findByRole('tab', { name: 'Remote HTTP' });
    await user.type(screen.getByLabelText('HTTPS endpoint'), 'https://mcp.example.com/v1');
    await user.click(screen.getByRole('button', { name: 'Create connection plan' }));

    expect(screen.queryByLabelText(/token|password|header/i)).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/credential reference|bearer/i)).not.toBeInTheDocument();
    expect(screen.getByLabelText('HTTPS endpoint')).toHaveAttribute('type', 'url');
    expect(
      screen.getByText('This page currently adds only no-auth Remote HTTP endpoints.')
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        'For endpoints that require credentials, open the Custom MCP tab and add a Streamable HTTP connection with environment variables or headers.'
      )
    ).toBeInTheDocument();
    expect(createManualMcpPlan).toHaveBeenCalledWith({
      type: 'remote_http',
      endpoint: 'https://mcp.example.com/v1',
      auth: { type: 'none' },
    });
  });

  it('offers approved stdio source selection without command or local execution inputs', async () => {
    const user = userEvent.setup();
    withIntl(<ManualTab onTaskCreated={vi.fn()} />);
    await user.click(await screen.findByRole('tab', { name: 'Approved stdio provider' }));
    await screen.findByRole('radio', { name: /Approved source/i });

    expect(
      screen.queryByLabelText(/command|arguments|argv|environment|env|working directory|cwd/i)
    ).not.toBeInTheDocument();
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
  });

  it('renders Source & Policy as a local-only governed import surface without URL or command inputs', () => {
    withIntl(<SourcesPolicyTab state={policy} onImported={vi.fn()} onOpenDiscover={vi.fn()} />);

    expect(screen.getByText('Import a trusted local source')).toBeInTheDocument();
    expect(screen.getByText('Local import only')).toBeInTheDocument();
    expect(
      screen.getByText('HTTPS source import is not available in this phase')
    ).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Choose manifest file' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Import source' })).toBeDisabled();
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
    expect(
      screen.queryByLabelText(/https endpoint|url|command|arguments|argv|environment|cwd/i)
    ).not.toBeInTheDocument();
  });

  it('imports a local manifest through the typed adapter and refresh callback', async () => {
    const user = userEvent.setup();
    const onImported = vi.fn().mockResolvedValue(undefined);

    withIntl(<SourcesPolicyTab state={policy} onImported={onImported} onOpenDiscover={vi.fn()} />);

    await user.click(screen.getByRole('button', { name: 'Choose manifest file' }));
    await user.click(screen.getByRole('button', { name: 'Import source' }));

    expect(window.electron.selectFileOrDirectory).toHaveBeenCalled();
    expect(importGovernedMcpSource).toHaveBeenCalledWith({
      type: 'local_manifest',
      filePath: '/tmp/catalogs/manifest.json',
    });
    expect(onImported).toHaveBeenCalled();
    expect(await screen.findByText('Trusted source imported')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Open Discover' })).toBeInTheDocument();
  });

  it('previews a local trusted source before explicitly confirming its refresh registration', async () => {
    const user = userEvent.setup();
    const onImported = vi.fn().mockResolvedValue(undefined);

    withIntl(<SourcesPolicyTab state={policy} onImported={onImported} onOpenDiscover={vi.fn()} />);

    await user.click(screen.getByRole('button', { name: 'Choose source directory' }));
    await user.click(screen.getByRole('button', { name: 'Preview source' }));

    expect(window.electron.directoryChooser).toHaveBeenCalled();
    expect(prepareMcpSourceProvision).toHaveBeenCalledWith('/tmp/catalogs/source-alpha');
    expect(await screen.findByText('Review trusted source')).toBeInTheDocument();
    expect(screen.getByText('catalog.example.test')).toBeInTheDocument();
    expect(
      screen.queryByText(/source_provisioning_confirmation_not-for-display/i)
    ).not.toBeInTheDocument();
    expect(
      screen.queryByLabelText(/url|endpoint|command|argv|environment|secret/i)
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Confirm trusted source' }));

    expect(confirmMcpSourceProvision).toHaveBeenCalledWith({
      provisionId: 'source_provisioning_alpha',
      confirmationToken: 'source_provisioning_confirmation_not_for-display',
    });
    expect(onImported).toHaveBeenCalled();
    expect(await screen.findByText('Trusted source saved')).toBeInTheDocument();
    expect(
      screen.queryByText(/source_provisioning_confirmation_not-for-display/i)
    ).not.toBeInTheDocument();
  });

  it('discards a failed source confirmation and requires a fresh preview instead of reusing the token', async () => {
    const user = userEvent.setup();
    vi.mocked(confirmMcpSourceProvision).mockRejectedValueOnce(new Error('confirmation expired'));

    withIntl(<SourcesPolicyTab state={policy} onImported={vi.fn()} onOpenDiscover={vi.fn()} />);

    await user.click(screen.getByRole('button', { name: 'Choose source directory' }));
    await user.click(screen.getByRole('button', { name: 'Preview source' }));
    await user.click(await screen.findByRole('button', { name: 'Confirm trusted source' }));

    expect(await screen.findByRole('button', { name: 'Preview source again' })).toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: 'Confirm trusted source' })
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Preview source again' }));
    expect(prepareMcpSourceProvision).toHaveBeenCalledTimes(2);
  });

  it('uses roving radio keyboard navigation and announces source import success', async () => {
    const user = userEvent.setup();

    withIntl(<SourcesPolicyTab state={policy} onImported={vi.fn()} onOpenDiscover={vi.fn()} />);

    const [manifest, directory] = screen.getAllByRole('radio');
    manifest.focus();
    await user.keyboard('{ArrowDown}');

    expect(directory).toHaveAttribute('aria-checked', 'true');
    expect(directory).toHaveFocus();

    const chooseDirectoryButtons = screen.getAllByRole('button', {
      name: 'Choose source directory',
    });
    await user.click(chooseDirectoryButtons[chooseDirectoryButtons.length - 1]);
    await user.click(screen.getByRole('button', { name: 'Import source' }));
    const importSuccess = await screen.findByText('Trusted source imported');
    expect(importSuccess.closest('[role="status"]')).toBeInTheDocument();
  });

  it('refreshes only registered sources without exposing URL, command, env, or secret inputs', async () => {
    const user = userEvent.setup();
    const onImported = vi.fn().mockResolvedValue(undefined);

    withIntl(
      <SourcesPolicyTab
        state={refreshablePolicy}
        onImported={onImported}
        onOpenDiscover={vi.fn()}
      />
    );

    expect(
      screen.getByText('Refreshing a source only updates the local catalog')
    ).toBeInTheDocument();
    expect(
      screen.queryByLabelText(/url|endpoint|command|argv|environment|secret/i)
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Refresh source' }));

    expect(refreshMcpSource).toHaveBeenCalledWith('verified_source_catalog_source-alpha');
    expect(onImported).toHaveBeenCalled();
    expect(await screen.findByText('Source catalog refreshed')).toBeInTheDocument();
    expect(
      screen.getByText(
        'The registered source was re-verified and the local Discover catalog was updated. No plan, install, or enable action was triggered.'
      )
    ).toBeInTheDocument();
  });

  it('reloads source policy after a refresh failure', async () => {
    const user = userEvent.setup();
    const onImported = vi.fn().mockResolvedValue(undefined);
    vi.mocked(refreshMcpSource).mockRejectedValueOnce(new Error('refresh failed'));

    withIntl(
      <SourcesPolicyTab
        state={refreshablePolicy}
        onImported={onImported}
        onOpenDiscover={vi.fn()}
      />
    );

    await user.click(screen.getByRole('button', { name: 'Refresh source' }));

    expect(onImported).toHaveBeenCalledTimes(1);
    expect(
      await screen.findByText('The MCP Platform request could not be completed.')
    ).toBeInTheDocument();
  });

  it('does not pass raw service envelope secrets into the recovery panel', () => {
    const recovery = mapMcpPlatformError({
      code: 'invalid_request',
      message:
        'Task monitor rejected the response. Authorization: Basic dXNlcjpwYXNz ' +
        'prefix Authorization: Bearer prefix-secret status=418 ' +
        'Authorization：Bearer fullwidth-secret ' +
        'Proxy-Authorization: Digest user=alice response=abc123 nonce=qwerty status=401 ' +
        'env={"TOKEN":"abc"} argv=["C:\\\\secret\\\\tool.exe"] command="/bin/sh -lc curl ws://private.example.test/socket" ' +
        '{"Authοrization":"Bearer greek-omicron-secret","status":"401"} ' +
        '{"Prοxy-Authorization":"Negotiate bypass-proxy-omicron","status":"402"} ' +
        '{"A\u200Buthorization":"Bearer zwsp-secret","status":"403"} ' +
        '{"A\u202Euthorization":"Bearer bidi-secret","status":"404"} ' +
        '{"A\u0000uthorization":"Bearer nul-secret","status":"405"} ' +
        '「Authorization」：「Bearer corner-secret」 ﹁Proxy-Authorization﹂＝﹁Negotiate small-form-secret tail-token﹂ ' +
        'path=/tmp Failure while reading /tmp, then (wss://private.example.test/stream).',
      retryable: true,
      correlationId: 'correlation-6',
    });

    withIntl(<RecoveryPanel recovery={recovery} />);

    expect(screen.getByRole('alert')).toHaveTextContent('Task monitor rejected the response.');
    expect(screen.getByRole('alert')).not.toHaveTextContent('dXNlcjpwYXNz');
    expect(screen.getByRole('alert')).not.toHaveTextContent('prefix-secret');
    expect(screen.getByRole('alert')).not.toHaveTextContent('fullwidth-secret');
    expect(screen.getByRole('alert')).not.toHaveTextContent('abc123');
    expect(screen.getByRole('alert')).not.toHaveTextContent('qwerty');
    expect(screen.getByRole('alert')).not.toHaveTextContent('greek-omicron-secret');
    expect(screen.getByRole('alert')).not.toHaveTextContent('bypass-proxy-omicron');
    expect(screen.getByRole('alert')).not.toHaveTextContent('zwsp-secret');
    expect(screen.getByRole('alert')).not.toHaveTextContent('bidi-secret');
    expect(screen.getByRole('alert')).not.toHaveTextContent('nul-secret');
    expect(screen.getByRole('alert')).not.toHaveTextContent('corner-secret');
    expect(screen.getByRole('alert')).not.toHaveTextContent('small-form-secret');
    expect(screen.getByRole('alert')).not.toHaveTextContent('private.example.test');
    expect(screen.getByRole('alert')).not.toHaveTextContent('path=/tmp');
    expect(screen.getByRole('alert')).not.toHaveTextContent('C:\\secret\\tool.exe');
    expect(screen.getByRole('alert')).toHaveTextContent('Authorization: Basic [redacted]');
    expect(screen.getByRole('alert')).toHaveTextContent('Authorization: Bearer [redacted]');
    expect(screen.getByRole('alert')).toHaveTextContent('Authorization： Bearer [redacted]');
    expect(screen.getByRole('alert')).toHaveTextContent('Proxy-Authorization: Digest [redacted]');
    expect(screen.getByRole('alert')).toHaveTextContent(
      '{"Authorization":"Bearer [redacted]","status":"401"}'
    );
    expect(screen.getByRole('alert')).toHaveTextContent(
      '{"Proxy-Authorization":"Negotiate [redacted]","status":"402"}'
    );
    expect(screen.getByRole('alert')).toHaveTextContent(
      '{"Authorization":"Bearer [redacted]","status":"403"}'
    );
    expect(screen.getByRole('alert')).toHaveTextContent(
      '{"Authorization":"Bearer [redacted]","status":"404"}'
    );
    expect(screen.getByRole('alert')).toHaveTextContent(
      '{"Authorization":"Bearer [redacted]","status":"405"}'
    );
    expect(screen.getByRole('alert')).toHaveTextContent('「Authorization」：「Bearer [redacted]」');
    expect(screen.getByRole('alert')).toHaveTextContent(
      '﹁Proxy-Authorization﹂＝﹁Negotiate [redacted]﹂'
    );
    expect(screen.getByRole('alert')).toHaveTextContent('path=[path removed]');
    expect(screen.getByRole('alert')).toHaveTextContent(
      'Failure while reading [path removed], then ([address removed]).'
    );
    expect(screen.getByRole('alert')).toHaveTextContent('Reference: correlation-6');
  });

  it('keeps the credential attention copy free of secret-bearing or opaque-boundary terms', () => {
    withIntl(<CredentialAttentionPanel status="trusted_state_conflict" />);

    const alert = screen.getByRole('alert');

    expect(alert).toHaveTextContent('Authentication state conflict');
    expect(alert).toHaveTextContent('Authentication entry not yet available');
    expect(alert).not.toHaveTextContent(/secret|token|password|header/i);
    expect(alert).not.toHaveTextContent(/handle|digest|witness|keyring|anchor|root|mac/i);
  });

  it('does not leak task error secrets through the actual managed task recovery panel', async () => {
    const failedTask = task({
      outcome: {
        state: 'failed',
        error: {
          code: 'adapter_failed',
          retryable: false,
          correlationId: 'task-security-ref',
          message:
            'Task failed. prefix Authorization: Bearer prefix-secret status=418 ' +
            'Authorization：Bearer fullwidth-secret ' +
            'Proxy-Authorization: Negotiate user=alice response=abc123 nonce=qwerty nextStep=Retry ' +
            '{"Authοrization":"Bearer greek-omicron-secret","status":"401"} ' +
            '{"A\u000Buthorization":"Bearer vt-secret","status":"402"} ' +
            '{"A\u0301uthorization":"Bearer combining-secret","status":"403"} path=/tmp ' +
            'Failure while reading /tmp, then (ws://private.example.test/socket).',
          suggestion: 'retry',
        },
        rollback: 'not_required',
        finalization: 'complete',
        remainingEffects: [],
        nextAction: 'wait',
      },
    });
    const item = summary('managed-security', 2, { currentTask: failedTask });
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [item], nextCursor: null });
    vi.mocked(getManagedMcp).mockResolvedValue(detail(item));
    vi.mocked(getMcpTask).mockResolvedValue(failedTask);
    vi.mocked(getMcpHealth).mockResolvedValue(health(item.managedMcpId));
    const user = userEvent.setup();

    withIntl(<ManagedTab externalTask={null} onTaskChange={vi.fn()} />);
    await user.click(await screen.findByRole('button', { name: /Security MCP/i }));

    const alerts = await screen.findAllByRole('alert');
    const taskAlert = alerts[alerts.length - 1];

    expect(taskAlert).toHaveTextContent('Task failed.');
    expect(taskAlert).toHaveTextContent('Authorization: Bearer [redacted]');
    expect(taskAlert).toHaveTextContent('Authorization： Bearer [redacted]');
    expect(taskAlert).toHaveTextContent('Proxy-Authorization: Negotiate [redacted]');
    expect(taskAlert).toHaveTextContent('{"Authorization":"Bearer [redacted]","status":"401"}');
    expect(taskAlert).toHaveTextContent('{"Authorization":"Bearer [redacted]","status":"402"}');
    expect(taskAlert).toHaveTextContent('{"Authorization":"Bearer [redacted]","status":"403"}');
    expect(taskAlert).toHaveTextContent('path=[path removed]');
    expect(taskAlert).toHaveTextContent(
      'Failure while reading [path removed], then ([address removed]).'
    );
    expect(taskAlert).toHaveTextContent('Reference: task-security-ref');
    expect(taskAlert).not.toHaveTextContent('prefix-secret');
    expect(taskAlert).not.toHaveTextContent('fullwidth-secret');
    expect(taskAlert).not.toHaveTextContent('abc123');
    expect(taskAlert).not.toHaveTextContent('qwerty');
    expect(taskAlert).not.toHaveTextContent('greek-omicron-secret');
    expect(taskAlert).not.toHaveTextContent('vt-secret');
    expect(taskAlert).not.toHaveTextContent('combining-secret');
    expect(taskAlert).not.toHaveTextContent('path=/tmp');
    expect(taskAlert).not.toHaveTextContent('private.example.test');
  });

  it('sanitizes a real monitor rejection before it reaches the recovery panel', async () => {
    const runningTask: McpTaskRef = {
      taskId: 'task-monitor-unicode',
      operation: 'update',
      status: 'running',
      progress: 45,
      cancellable: true,
      revision: 3,
      updatedAtMs: 3,
      outcome: {
        state: 'pending',
        rollback: 'not_required',
        finalization: 'pending',
        remainingEffects: [],
        nextAction: 'wait',
      },
    };
    const item = summary('managed-security', 2, { currentTask: runningTask });
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [item], nextCursor: null });
    vi.mocked(getManagedMcp).mockResolvedValue(detail(item));
    vi.mocked(getMcpHealth).mockResolvedValue(health(item.managedMcpId));
    vi.mocked(getMcpTask).mockRejectedValueOnce(
      new Error(
        'Task monitor rejected the response. prefix Authorization: Bearer prefix-secret status=418 ' +
          'Authorization：Bearer fullwidth-secret ' +
          '{"A\u0000uthorization":"Bearer nul-secret","status":"401"} ' +
          'Proxy-Authorization: Digest user=alice response=abc123 nonce=qwerty status=402 ' +
          '﹁Proxy-Authorization﹂＝﹁Negotiate small-form-secret tail-token﹂'
      )
    );
    const user = userEvent.setup();

    withIntl(<ManagedTab externalTask={null} onTaskChange={vi.fn()} />);
    await user.click(await screen.findByRole('button', { name: /Security MCP/i }));

    const monitorAlert = await screen.findByRole('alert');

    expect(monitorAlert).toHaveTextContent('The MCP Platform request could not be completed.');
    expect(monitorAlert).not.toHaveTextContent('Task monitor rejected the response.');
    expect(monitorAlert).not.toHaveTextContent('Authorization');
    expect(monitorAlert).not.toHaveTextContent('Proxy-Authorization');
    expect(monitorAlert).not.toHaveTextContent('prefix-secret');
    expect(monitorAlert).not.toHaveTextContent('fullwidth-secret');
    expect(monitorAlert).not.toHaveTextContent('nul-secret');
    expect(monitorAlert).not.toHaveTextContent('abc123');
    expect(monitorAlert).not.toHaveTextContent('qwerty');
    expect(monitorAlert).not.toHaveTextContent('small-form-secret');
    expect(await screen.findByRole('button', { name: 'Retry' })).toBeInTheDocument();
  });

  it('keeps default enable changes bound to the managed summary returned by the service', async () => {
    const item = summary('managed-security', 4);
    vi.mocked(listManagedMcps).mockResolvedValue({ items: [item], nextCursor: null });
    vi.mocked(getManagedMcp).mockResolvedValue(detail(item));
    vi.mocked(getMcpHealth).mockResolvedValue(health(item.managedMcpId));
    const user = userEvent.setup();

    withIntl(<ManagedTab externalTask={null} onTaskChange={vi.fn()} />);
    await user.click(await screen.findByRole('button', { name: /Security MCP/i }));
    await user.click(screen.getByRole('switch', { name: 'Enabled by default for new sessions' }));

    expect(setMcpDefaultEnabled).toHaveBeenCalledWith(
      expect.objectContaining({
        managedMcpId: 'managed-security',
        revision: 4,
        defaultEnabled: false,
      }),
      true
    );
    expect(setMcpDefaultEnabled).not.toHaveBeenCalledWith(
      expect.objectContaining({
        sourceId: expect.anything(),
        manifestDigest: expect.anything(),
      }),
      true
    );
  });
});
