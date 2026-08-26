import { IntlProvider } from 'react-intl';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  LuminaClient,
  type McpCatalogPage,
  type McpCatalogSummary,
  type McpGovernedImportResult,
  type McpSourcesPolicyState,
  type Stream,
} from '@hikerm/lumina-sdk';
import McpCenterView from '../McpCenterView';

const boundary = vi.hoisted(() => ({ client: null as LuminaClient | null }));

vi.mock('../../../acp/acpConnection', () => ({
  getAcpClient: async () => {
    if (!boundary.client) throw new Error('desktop boundary client is not configured');
    return boundary.client;
  },
}));

type RpcResult = Record<string, unknown>;
type RpcHandler = (params: Record<string, unknown>) => RpcResult | Promise<RpcResult>;

class DeferredAcpTransport {
  readonly calls: Array<{ method: string; params: Record<string, unknown> }> = [];
  readonly handlers = new Map<string, RpcHandler>();
  readonly stream: Stream;
  private closeReadable!: () => void;

  constructor() {
    let respond!: (id: string | number, result: RpcResult) => void;
    const readable: Stream['readable'] = new globalThis.ReadableStream({
      start: (controller) => {
        respond = (id, result) => controller.enqueue({ jsonrpc: '2.0', id, result });
        this.closeReadable = () => controller.close();
      },
    });
    const writable: Stream['writable'] = new globalThis.WritableStream({
      write: (message) => {
        if (!('method' in message) || !('id' in message) || message.id === null) return;
        const id = message.id;
        const params = Object.fromEntries(Object.entries(message.params ?? {}));
        this.calls.push({ method: message.method, params });
        const handler = this.handlers.get(message.method);
        if (!handler) throw new Error(`Unhandled ACP method: ${message.method}`);
        void Promise.resolve(handler(params)).then((result) => respond(id, result));
      },
    });
    this.stream = { readable, writable };
  }

  close() {
    this.closeReadable();
  }
}

function success(value: unknown): RpcResult {
  return { outcome: { status: 'success', value } };
}

function policy(imported: boolean, refreshable = false): McpSourcesPolicyState {
  return {
    policy: {
      targetPlatform: 'windows',
      targetArchitecture: 'x86_64',
      developmentMode: false,
      dockerAllowed: false,
      recovery: 'none',
    },
    sources:
      imported || refreshable
        ? [
            {
              sourceId: refreshable
                ? 'verified_source_catalog_source-alpha'
                : 'local_manifest_source',
              displayName: refreshable ? 'Alpha Source' : 'Local manifest source',
              importKind: refreshable ? 'verified_source_catalog' : 'local_persistence',
              manifestCount: imported ? 1 : 0,
              compatibility: {
                compatible: imported ? 1 : 0,
                restricted: 0,
                denied: 0,
              },
              trustTiers: [refreshable ? 'community' : 'local'],
              cache: {
                offline: !refreshable,
                localPersistenceOnly: !refreshable,
                ...(imported ? { newestVerifiedAtMs: 1 } : {}),
                freshness: imported ? 'fresh' : 'empty',
                refreshState: refreshable ? 'idle' : 'local_only',
                recovery: 'none',
              },
              recovery: 'none',
              ...(imported
                ? {
                    lastImportedAtMs: 1,
                    lastRefreshDocumentDigest: 'document-alpha',
                    lastRefreshedAtMs: 1,
                  }
                : {}),
            },
          ]
        : [],
  };
}

function catalogItem(): McpCatalogSummary {
  return {
    sourceId: 'local_manifest_source',
    mcpId: 'pkg.local.import',
    version: '1.0.0',
    manifestDigest: 'manifest-digest-local-import',
    name: 'Local Import MCP',
    description: 'Imported through the governed local source flow.',
    publisherId: 'publisher',
    publisherName: 'Publisher',
    trustTier: 'local',
    proof: { type: 'local_bytes' },
    compatibility: 'compatible',
    distribution: 'remote_http',
    verifiedAtMs: 1,
    eligibility: { outcome: 'allowed', reason: 'eligible', recovery: 'none' },
  };
}

function catalogPage(imported: boolean): McpCatalogPage {
  return {
    items: imported ? [catalogItem()] : [],
    cache: {
      offline: false,
      localPersistenceOnly: true,
      ...(imported ? { newestVerifiedAtMs: 1 } : {}),
      freshness: imported ? 'fresh' : 'stale',
      refreshState: 'idle',
      recovery: 'none',
    },
  };
}

function importResult(): McpGovernedImportResult {
  return {
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
    entries: [catalogItem()],
  };
}

function sourceRefreshResult() {
  return {
    sourceId: 'verified_source_catalog_source-alpha',
    displayName: 'Alpha Source',
    documentDigest: 'document-alpha',
    manifestCount: 1,
    refreshedAtMs: 2,
  };
}

function renderView() {
  render(
    <IntlProvider locale="en">
      <McpCenterView />
    </IntlProvider>
  );
}

describe('McpCenterView through the real Desktop adapter and SDK client', () => {
  let transport: DeferredAcpTransport;

  beforeEach(() => {
    transport = new DeferredAcpTransport();
    boundary.client = new LuminaClient(
      () => ({
        requestPermission: async () => {
          throw new Error('unexpected permission request');
        },
        sessionUpdate: async () => {},
      }),
      transport.stream
    );

    let imported = false;
    let refreshable = false;
    transport.handlers.set('lumina.mcpSourcesPolicyGet_unstable', () =>
      success(policy(imported, refreshable))
    );
    transport.handlers.set('lumina.mcpCatalogList_unstable', () => success(catalogPage(imported)));
    transport.handlers.set('lumina.mcpGovernedImport_unstable', () => {
      imported = true;
      return success(importResult());
    });
    transport.handlers.set('lumina.mcpSourceRefresh_unstable', () => {
      imported = true;
      refreshable = true;
      return success(sourceRefreshResult());
    });

    window.electron.selectFileOrDirectory = vi.fn().mockResolvedValue('D:/catalogs/manifest.json');
  });

  afterEach(() => {
    boundary.client = null;
    transport.close();
  });

  it('imports a governed local source, refreshes Discover, and reads the new catalog via formal ACP list', async () => {
    const user = userEvent.setup();

    renderView();

    await waitFor(() => {
      expect(
        transport.calls.filter(({ method }) => method === 'lumina.mcpCatalogList_unstable')
      ).toHaveLength(1);
    });

    await user.click(screen.getByRole('tab', { name: 'Source & Policy' }));
    await user.click(await screen.findByRole('button', { name: 'Choose manifest file' }));
    await user.click(screen.getByRole('button', { name: 'Import source' }));

    expect(window.electron.selectFileOrDirectory).toHaveBeenCalledTimes(1);
    expect(await screen.findByText('Trusted source imported')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Open Discover' }));
    await waitFor(() => {
      expect(
        transport.calls.filter(({ method }) => method === 'lumina.mcpCatalogList_unstable')
      ).toHaveLength(2);
    });

    expect(await screen.findByRole('heading', { name: 'Local Import MCP' })).toBeInTheDocument();
    expect(transport.calls).toEqual(
      expect.arrayContaining([
        {
          method: 'lumina.mcpGovernedImport_unstable',
          params: {
            source: {
              type: 'local_manifest',
              filePath: 'D:/catalogs/manifest.json',
            },
          },
        },
        {
          method: 'lumina.mcpCatalogList_unstable',
          params: {
            pageSize: 24,
          },
        },
      ])
    );
    expect(transport.calls.some(({ method }) => method === 'lumina.mcpPlanCreate_unstable')).toBe(
      false
    );
    expect(
      transport.calls.some(({ method }) => method === 'lumina.mcpInstallConfirm_unstable')
    ).toBe(false);
    expect(
      transport.calls.some(({ method }) => method === 'lumina.mcpSetDefaultEnabled_unstable')
    ).toBe(false);
  });

  it('refreshes a registered source through formal ACP without creating plans or enabling MCPs', async () => {
    const user = userEvent.setup();

    transport.handlers.set('lumina.mcpSourcesPolicyGet_unstable', () =>
      success(policy(false, true))
    );
    transport.handlers.set('lumina.mcpCatalogList_unstable', () => success(catalogPage(false)));
    transport.handlers.set('lumina.mcpSourceRefresh_unstable', () => {
      transport.handlers.set('lumina.mcpSourcesPolicyGet_unstable', () =>
        success(policy(true, true))
      );
      transport.handlers.set('lumina.mcpCatalogList_unstable', () => success(catalogPage(true)));
      return success(sourceRefreshResult());
    });

    renderView();

    await user.click(await screen.findByRole('tab', { name: 'Source & Policy' }));
    await user.click(await screen.findByRole('button', { name: 'Refresh source' }));

    expect(await screen.findByText('Source catalog refreshed')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Open Discover' }));
    expect(await screen.findByRole('heading', { name: 'Local Import MCP' })).toBeInTheDocument();

    expect(transport.calls).toEqual(
      expect.arrayContaining([
        {
          method: 'lumina.mcpSourceRefresh_unstable',
          params: {
            sourceId: 'verified_source_catalog_source-alpha',
          },
        },
      ])
    );
    expect(transport.calls.some(({ method }) => method === 'lumina.mcpPlanCreate_unstable')).toBe(
      false
    );
    expect(
      transport.calls.some(({ method }) => method === 'lumina.mcpInstallConfirm_unstable')
    ).toBe(false);
    expect(
      transport.calls.some(({ method }) => method === 'lumina.mcpSetDefaultEnabled_unstable')
    ).toBe(false);
  });
});
