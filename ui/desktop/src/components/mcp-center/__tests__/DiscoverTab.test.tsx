import { IntlProvider } from 'react-intl';
import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { McpCatalogDetail, McpCatalogPage, McpCatalogSummary } from '@aaif/goose-sdk';
import { DiscoverTab } from '../DiscoverTab';
import { getMcpCatalogDetail, listMcpCatalog } from '../../../acp/mcp-platform';

vi.mock('../../../acp/mcp-platform', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../../acp/mcp-platform')>();
  return {
    ...actual,
    getMcpCatalogDetail: vi.fn(),
    listMcpCatalog: vi.fn(),
  };
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

const summary = (id: string): McpCatalogSummary => ({
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
});

const page = (item: McpCatalogSummary): McpCatalogPage => ({
  items: [item],
  cache: {
    offline: false,
    localPersistenceOnly: true,
    freshness: 'fresh',
    refreshState: 'idle',
    recovery: 'none',
  },
});

const detail = (item: McpCatalogSummary): McpCatalogDetail => ({
  sourceId: item.sourceId,
  mcpId: item.mcpId,
  version: item.version,
  manifestDigest: item.manifestDigest,
  name: item.name,
  description: item.description,
  publisher: { id: 'publisher', name: 'Publisher', signingIdentities: [] },
  proof: item.proof,
  trustTier: item.trustTier,
  distribution: { type: 'remote_http' },
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
  verifiedAtMs: 1,
  eligibility: item.eligibility,
});

function renderDiscover() {
  render(
    <IntlProvider locale="en">
      <DiscoverTab sourcesPolicy={null} onTaskCreated={vi.fn()} />
    </IntlProvider>
  );
}

describe('DiscoverTab request freshness', () => {
  beforeEach(() => vi.clearAllMocks());

  it('never lets an older search overwrite the current query results', async () => {
    const itemA = summary('A');
    const itemB = summary('B');
    const pendingA = deferred<McpCatalogPage>();
    vi.mocked(listMcpCatalog).mockImplementation((request) => {
      if (request.query === 'a') return pendingA.promise;
      if (request.query === 'b') return Promise.resolve(page(itemB));
      return Promise.resolve({ ...page(itemA), items: [] });
    });
    const user = userEvent.setup();
    renderDiscover();

    const search = screen.getByPlaceholderText('Search MCPs');
    await user.type(search, 'a');
    await user.clear(search);
    await user.type(search, 'b');
    expect(await screen.findByRole('heading', { name: 'MCP B' })).toBeInTheDocument();
    await act(async () => pendingA.resolve(page(itemA)));

    expect(screen.getByRole('heading', { name: 'MCP B' })).toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: 'MCP A' })).not.toBeInTheDocument();
  });

  it('never lets an older catalog detail replace the current selection', async () => {
    const itemA = summary('A');
    const itemB = summary('B');
    const pendingA = deferred<McpCatalogDetail>();
    vi.mocked(listMcpCatalog).mockResolvedValue({
      ...page(itemA),
      items: [itemA, itemB],
    });
    vi.mocked(getMcpCatalogDetail).mockImplementation((digest) =>
      digest === itemA.manifestDigest ? pendingA.promise : Promise.resolve(detail(itemB))
    );
    const user = userEvent.setup();
    renderDiscover();

    await user.click(await screen.findByRole('button', { name: /MCP A/i }));
    await user.click(screen.getByRole('button', { name: /MCP B/i }));
    expect(await screen.findByRole('heading', { level: 2, name: 'MCP B' })).toBeInTheDocument();
    await act(async () => pendingA.resolve(detail(itemA)));

    expect(screen.getByRole('heading', { level: 2, name: 'MCP B' })).toBeInTheDocument();
  });
});
