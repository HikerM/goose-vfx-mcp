import { IntlProvider } from 'react-intl';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type {
  McpCatalogDetail,
  McpCatalogPage,
  McpCatalogSummary,
  McpPlanReview,
  McpSourcesPolicyState,
} from '@aaif/goose-sdk';
import { DiscoverTab } from '../DiscoverTab';
import { createMcpPlan, getMcpCatalogDetail, listMcpCatalog } from '../../../acp/mcp-platform';

type CatalogBoundPlanReview = McpPlanReview & {
  catalogTarget?: {
    sourceId: string;
    mcpId: string;
    version: string;
    manifestDigest: string;
  };
};

vi.mock('../../../acp/mcp-platform', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../../acp/mcp-platform')>();
  return {
    ...actual,
    createMcpPlan: vi.fn(),
    getMcpCatalogDetail: vi.fn(),
    listMcpCatalog: vi.fn(),
  };
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });
  return { promise, resolve, reject };
}

function setViewport(width: number) {
  Object.defineProperty(window, 'innerWidth', {
    configurable: true,
    writable: true,
    value: width,
  });
  window.dispatchEvent(new Event('resize'));
}

const summary = (id: string, overrides: Partial<McpCatalogSummary> = {}): McpCatalogSummary => ({
  sourceId: 'catalog',
  mcpId: id,
  version: '1.0.0',
  manifestDigest: `digest-${id}`,
  name: `MCP ${id}`,
  description: `${id} description`,
  publisherId: 'publisher',
  publisherName: 'Publisher',
  trustTier: 'official',
  proof: { type: 'local_bytes' },
  compatibility: 'compatible',
  distribution: 'remote_http',
  verifiedAtMs: 1,
  eligibility: { outcome: 'allowed', reason: 'eligible', recovery: 'none' },
  ...overrides,
});

const page = (
  items: McpCatalogSummary[],
  overrides: Partial<McpCatalogPage> = {}
): McpCatalogPage => ({
  items,
  cache: {
    offline: false,
    localPersistenceOnly: true,
    newestVerifiedAtMs: 1,
    freshness: 'fresh',
    refreshState: 'idle',
    recovery: 'none',
  },
  ...overrides,
});

const detail = (item: McpCatalogSummary): McpCatalogDetail => ({
  sourceId: item.sourceId,
  mcpId: item.mcpId,
  version: item.version,
  manifestDigest: item.manifestDigest,
  name: item.name,
  description: item.description,
  publisher: { id: 'publisher', name: item.publisherName, signingIdentities: [] },
  proof: item.proof,
  trustTier: item.trustTier,
  distribution:
    item.distribution === 'npm'
      ? {
          type: 'npm',
          package: '@scope/example',
          package_version: item.version,
          artifacts: [],
        }
      : { type: 'remote_http' },
  transport: {
    type: 'streamable_http',
    endpoint: 'https://example.test',
    allowed_redirect_origins: [],
  },
  auth: { type: 'none' },
  health: { type: 'http', relative_endpoint: '/health', expected_status: 200, timeout_seconds: 5 },
  capabilities: ['tools'],
  permissions: [],
  compatibility: item.compatibility,
  verifiedAtMs: item.verifiedAtMs,
  eligibility: item.eligibility,
});

const plan = (
  item: McpCatalogSummary,
  overrides: Partial<CatalogBoundPlanReview> = {}
): CatalogBoundPlanReview => ({
  planId: 'plan-1',
  planDigest: 'digest-1',
  expiresAtMs: Date.now() + 60_000,
  sourceId: item.sourceId,
  catalogTarget: {
    sourceId: item.sourceId,
    mcpId: item.mcpId,
    version: item.version,
    manifestDigest: item.manifestDigest,
  },
  proof: { type: 'local_bytes' },
  trustTier: item.trustTier,
  publisher: { id: 'publisher', name: item.publisherName, signingIdentities: [] },
  mcpId: item.mcpId,
  name: item.name,
  version: item.version,
  selectedManifestDigest: item.manifestDigest,
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
  ...overrides,
});

const policy = (overrides: Partial<McpSourcesPolicyState> = {}): McpSourcesPolicyState => ({
  policy: {
    targetPlatform: 'windows',
    targetArchitecture: 'x86_64',
    developmentMode: false,
    dockerAllowed: false,
    recovery: 'none',
  },
  sources: [],
  ...overrides,
});

function renderDiscover(sourcesPolicy: McpSourcesPolicyState | null = null, refreshNonce = 0) {
  render(
    <IntlProvider locale="en">
      <DiscoverTab
        sourcesPolicy={sourcesPolicy}
        refreshNonce={refreshNonce}
        onTaskCreated={vi.fn()}
      />
    </IntlProvider>
  );
}

describe('DiscoverTab', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    setViewport(1440);
    vi.mocked(createMcpPlan).mockResolvedValue(plan(summary('default')));
  });

  it('describes Discover as a verified offline catalog instead of live internet search', async () => {
    vi.mocked(listMcpCatalog).mockResolvedValue(page([]));

    renderDiscover(policy());

    expect(
      screen.getByText(
        'This device-only catalog shows MCPs that have already been verified and stored locally. Filters narrow local results only; this page does not search the internet.'
      )
    ).toBeInTheDocument();
    expect(screen.getByPlaceholderText('Filter local catalog')).toBeInTheDocument();
    expect(screen.queryByPlaceholderText('Search MCPs')).not.toBeInTheDocument();
  });

  it('shows cache freshness guidance without replacing verified results', async () => {
    const item = summary('A');
    vi.mocked(listMcpCatalog).mockResolvedValue(
      page([item], {
        cache: {
          offline: true,
          localPersistenceOnly: true,
          newestVerifiedAtMs: 1,
          freshness: 'offline_verified',
          refreshState: 'idle',
          recovery: 'none',
        },
      })
    );

    renderDiscover(policy());

    expect(await screen.findByRole('heading', { name: 'MCP A' })).toBeInTheDocument();
    expect(screen.getByText('Using a verified offline local cache')).toBeInTheDocument();
    expect(screen.getByText(/Last verified:/)).toBeInTheDocument();
  });

  it('keeps last-known-good catalog items visible when a later refresh fails', async () => {
    const item = summary('A');
    vi.mocked(listMcpCatalog)
      .mockResolvedValueOnce(
        page([item], {
          nextCursor: 'page-2',
        })
      )
      .mockRejectedValueOnce(new Error('refresh failed'));
    const user = userEvent.setup();

    renderDiscover(policy());

    expect(await screen.findByRole('heading', { name: 'MCP A' })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Load more' }));

    expect(screen.getByRole('heading', { name: 'MCP A' })).toBeInTheDocument();
    expect(await screen.findByRole('button', { name: 'Retry' })).toBeInTheDocument();
    expect(screen.getByRole('alert')).toHaveTextContent('Catalog refresh failed');
  });

  it('never lets an older search overwrite the current query results', async () => {
    const itemA = summary('A');
    const itemB = summary('B');
    const pendingA = deferred<McpCatalogPage>();
    vi.mocked(listMcpCatalog).mockImplementation((request) => {
      if (request.query === 'a') return pendingA.promise;
      if (request.query === 'b') return Promise.resolve(page([itemB]));
      return Promise.resolve(page([]));
    });
    const user = userEvent.setup();

    renderDiscover();

    const search = screen.getByPlaceholderText('Filter local catalog');
    await user.type(search, 'a');
    await user.clear(search);
    await user.type(search, 'b');
    expect(await screen.findByRole('heading', { name: 'MCP B' })).toBeInTheDocument();

    await act(async () => pendingA.resolve(page([itemA])));

    expect(screen.getByRole('heading', { name: 'MCP B' })).toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: 'MCP A' })).not.toBeInTheDocument();
  });

  it('refreshes the catalog when the parent signals a source import refresh', async () => {
    vi.mocked(listMcpCatalog)
      .mockResolvedValueOnce(page([summary('A')]))
      .mockResolvedValueOnce(page([summary('B')]));

    const { rerender } = render(
      <IntlProvider locale="en">
        <DiscoverTab sourcesPolicy={policy()} refreshNonce={0} onTaskCreated={vi.fn()} />
      </IntlProvider>
    );

    expect(await screen.findByRole('heading', { name: 'MCP A' })).toBeInTheDocument();

    rerender(
      <IntlProvider locale="en">
        <DiscoverTab sourcesPolicy={policy()} refreshNonce={1} onTaskCreated={vi.fn()} />
      </IntlProvider>
    );

    expect(await screen.findByRole('heading', { name: 'MCP B' })).toBeInTheDocument();
    expect(listMcpCatalog).toHaveBeenCalledTimes(2);
  });

  it('uses source-aware detail locators and ignores an older response for a previous selection', async () => {
    const itemA = summary('shared', {
      sourceId: 'catalog-a',
      manifestDigest: 'digest-shared',
      publisherName: 'Publisher A',
    });
    const itemB = summary('shared', {
      sourceId: 'catalog-b',
      manifestDigest: 'digest-shared',
      publisherName: 'Publisher B',
    });
    const pendingA = deferred<McpCatalogDetail>();
    vi.mocked(listMcpCatalog).mockResolvedValue(page([itemA, itemB]));
    vi.mocked(getMcpCatalogDetail).mockImplementation((locator) => {
      const ref = locator as unknown as { sourceId: string; mcpId: string; version: string };
      if (ref.sourceId === 'catalog-a') {
        return pendingA.promise;
      }
      return Promise.resolve(detail(itemB));
    });
    const user = userEvent.setup();

    renderDiscover();

    const sourceAButton = await screen.findByRole('button', { name: /catalog-a/i });
    const sourceBButton = screen.getByRole('button', { name: /catalog-b/i });
    await user.click(sourceAButton);
    await user.click(sourceBButton);

    expect(getMcpCatalogDetail).toHaveBeenNthCalledWith(1, {
      sourceId: 'catalog-a',
      mcpId: 'shared',
      version: '1.0.0',
    });
    expect(getMcpCatalogDetail).toHaveBeenNthCalledWith(2, {
      sourceId: 'catalog-b',
      mcpId: 'shared',
      version: '1.0.0',
    });
    expect(sourceAButton).toHaveAttribute('aria-pressed', 'false');
    expect(sourceBButton).toHaveAttribute('aria-pressed', 'true');

    await act(async () => pendingA.resolve(detail(itemA)));

    expect(screen.getByRole('heading', { level: 2, name: 'MCP shared' })).toBeInTheDocument();
    expect(screen.getByText('Source')).toBeInTheDocument();
    expect(screen.getByText('catalog-b')).toBeInTheDocument();
    expect(screen.getByLabelText('MCP details')).toHaveClass(
      'min-[1200px]:sticky',
      'min-[1200px]:max-h-[calc(100dvh-190px)]'
    );
  });

  it('fails closed when a detail response identity does not match the selected catalog card', async () => {
    const itemA = summary('shared', {
      sourceId: 'catalog-a',
      manifestDigest: 'digest-a',
    });
    vi.mocked(listMcpCatalog).mockResolvedValue(page([itemA]));
    vi.mocked(getMcpCatalogDetail).mockResolvedValue(
      detail(
        summary('shared', {
          sourceId: 'catalog-b',
          manifestDigest: 'digest-b',
        })
      )
    );
    const user = userEvent.setup();

    renderDiscover();

    await user.click(await screen.findByRole('button', { name: /catalog-a/i }));

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('The selected MCP detail could not be verified');
    expect(alert).toHaveTextContent(
      'MCP Center blocked this detail response because it did not match the exact catalog item you selected.'
    );
    expect(screen.queryByRole('heading', { level: 2, name: 'MCP shared' })).not.toBeInTheDocument();
  });

  it('keeps the last safe detail visible when the same selection refresh fails', async () => {
    const itemA = summary('A');
    vi.mocked(listMcpCatalog).mockResolvedValue(page([itemA]));
    vi.mocked(getMcpCatalogDetail)
      .mockResolvedValueOnce(detail(itemA))
      .mockRejectedValueOnce(new Error('refresh failed'));
    const user = userEvent.setup();

    renderDiscover();

    const itemButton = await screen.findByRole('button', { name: /MCP A/i });
    await user.click(itemButton);
    expect(await screen.findByRole('heading', { level: 2, name: 'MCP A' })).toBeInTheDocument();

    await user.click(itemButton);

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'The MCP Platform request could not be completed.'
    );
    expect(screen.getByRole('heading', { level: 2, name: 'MCP A' })).toBeInTheDocument();
  });

  it('does not show a previous item detail when a different selection fails to refresh', async () => {
    const itemA = summary('A');
    const itemB = summary('B', { sourceId: 'catalog-b', manifestDigest: 'digest-b' });
    vi.mocked(listMcpCatalog).mockResolvedValue(page([itemA, itemB]));
    vi.mocked(getMcpCatalogDetail)
      .mockResolvedValueOnce(detail(itemA))
      .mockRejectedValueOnce(new Error('selection failed'));
    const user = userEvent.setup();

    renderDiscover();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    expect(await screen.findByRole('heading', { level: 2, name: 'MCP A' })).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: /catalog-b/i }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'The MCP Platform request could not be completed.'
    );
    expect(screen.queryByRole('heading', { level: 2, name: 'MCP A' })).not.toBeInTheDocument();
  });

  it('creates a register_catalog plan with the full catalog target for registration-only items', async () => {
    const item = summary('remote', {
      sourceId: 'catalog-a',
      manifestDigest: 'digest-remote',
      distribution: 'remote_http',
    });
    vi.mocked(listMcpCatalog).mockResolvedValue(page([item]));
    vi.mocked(getMcpCatalogDetail).mockResolvedValue(detail(item));
    vi.mocked(createMcpPlan).mockResolvedValue(plan(item));
    const user = userEvent.setup();

    renderDiscover();

    await user.click(await screen.findByRole('button', { name: /catalog-a/i }));
    await user.click(await screen.findByRole('button', { name: 'Create registration plan' }));

    expect(createMcpPlan).toHaveBeenCalledWith({
      type: 'register_catalog',
      catalog: {
        sourceId: 'catalog-a',
        mcpId: 'remote',
        version: '1.0.0',
        manifestDigest: 'digest-remote',
      },
      installation_scope: 'user',
    });
    expect(createMcpPlan).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: 'register', manifest_digest: 'digest-remote' })
    );
    expect(await screen.findByRole('dialog')).toHaveTextContent('Review MCP plan');
  });

  it('creates an install_catalog plan with the full catalog target for installable items', async () => {
    const item = summary('pkg', {
      sourceId: 'catalog-pkg',
      manifestDigest: 'digest-pkg',
      distribution: 'npm',
    });
    vi.mocked(listMcpCatalog).mockResolvedValue(page([item]));
    vi.mocked(getMcpCatalogDetail).mockResolvedValue(detail(item));
    vi.mocked(createMcpPlan).mockResolvedValue(plan(item));
    const user = userEvent.setup();

    renderDiscover();

    await user.click(await screen.findByRole('button', { name: /catalog-pkg/i }));
    await user.click(await screen.findByRole('button', { name: 'Create install plan' }));

    expect(createMcpPlan).toHaveBeenCalledWith({
      type: 'install_catalog',
      catalog: {
        sourceId: 'catalog-pkg',
        mcpId: 'pkg',
        version: '1.0.0',
        manifestDigest: 'digest-pkg',
      },
    });
    expect(createMcpPlan).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: 'install', manifest_digest: 'digest-pkg' })
    );
  });

  it('fails closed when plan review omits or mismatches the durable catalog target', async () => {
    const item = summary('A', {
      sourceId: 'catalog-a',
      manifestDigest: 'digest-a',
    });
    vi.mocked(listMcpCatalog).mockResolvedValue(page([item]));
    vi.mocked(getMcpCatalogDetail).mockResolvedValue(detail(item));
    vi.mocked(createMcpPlan).mockResolvedValue(
      plan(item, {
        catalogTarget: {
          sourceId: 'catalog-b',
          mcpId: 'A',
          version: '1.0.0',
          manifestDigest: 'digest-a',
        },
      })
    );
    const user = userEvent.setup();

    renderDiscover();

    await user.click(await screen.findByRole('button', { name: /catalog-a/i }));
    await user.click(await screen.findByRole('button', { name: 'Create registration plan' }));

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(
      'The reviewed plan no longer matches the selected catalog item'
    );
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('uses a focus-trapped medium drawer and restores focus on Escape', async () => {
    setViewport(900);
    const item = summary('A');
    vi.mocked(listMcpCatalog).mockResolvedValue(page([item]));
    vi.mocked(getMcpCatalogDetail).mockResolvedValue(detail(item));
    const user = userEvent.setup();

    renderDiscover();

    const card = await screen.findByRole('button', { name: /MCP A/i });
    await user.click(card);

    const drawer = await screen.findByRole('dialog');
    expect(within(drawer).getByText('MCP detail')).toBeInTheDocument();
    await user.keyboard('{Escape}');

    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(card).toHaveFocus();
  });

  it('uses a single-column detail view with Back on compact widths', async () => {
    setViewport(720);
    const item = summary('A');
    vi.mocked(listMcpCatalog).mockResolvedValue(page([item]));
    vi.mocked(getMcpCatalogDetail).mockResolvedValue(detail(item));
    const user = userEvent.setup();

    renderDiscover();

    const card = await screen.findByRole('button', { name: /MCP A/i });
    await user.click(card);

    expect(await screen.findByRole('button', { name: 'Back to results' })).toBeInTheDocument();
    expect(screen.queryByPlaceholderText('Filter local catalog')).not.toBeInTheDocument();
    expect(screen.getByRole('heading', { level: 2, name: 'MCP A' })).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Back to results' }));

    expect(await screen.findByPlaceholderText('Filter local catalog')).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole('button', { name: /MCP A/i })).toHaveFocus());
  });
});
