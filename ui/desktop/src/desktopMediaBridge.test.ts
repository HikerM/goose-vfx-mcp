import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { createHash } from 'node:crypto';
import fs from 'node:fs/promises';
import path from 'node:path';
import { DesktopMediaBridge, type DesktopMediaBridgeDependencies } from './desktopMediaBridge';
import type { LuminaServeLease } from './luminaServeLeaseRegistry';
import type { DesktopMediaGrant } from './types/desktopMedia';

const identity = { sessionId: 'session-1', resultId: '12345678-1234-4234-8234-123456789abc' };
const bytes = new TextEncoder().encode('RIFF-small-protocol-test');
const metadata = {
  fileName: 'output.wav',
  mime: 'audio/wav',
  bytes: bytes.length,
  sha256: createHash('sha256').update(bytes).digest('hex'),
};
const grant: DesktopMediaGrant = { ticket: 'a'.repeat(64), expiresInMs: 300000, metadata };
const headers = () => ({
  'Content-Type': metadata.mime,
  'Content-Length': String(bytes.length),
  ETag: `"${metadata.sha256}"`,
  'Accept-Ranges': 'bytes',
});
const root = path.resolve(
  process.cwd(),
  '../../target/lifecycle-validation/m4-desktop-media-bridge-tests'
);
let directory: string;
let source: string;
let lease: LuminaServeLease;
let deps: DesktopMediaBridgeDependencies;
let bridge: DesktopMediaBridge;
let cancelled: ReturnType<typeof vi.fn<() => void>>;

function content(_url: string, init: globalThis.RequestInit) {
  return Promise.resolve(
    new Response(init.method === 'HEAD' ? null : bytes, { headers: headers() })
  );
}
function heldResponse(value = { ...metadata, path: source }, end = false) {
  return new Response(
    new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(`${JSON.stringify(value)}\n`));
        if (end) controller.close();
      },
      cancel() {
        cancelled();
      },
    }),
    { headers: { 'Content-Type': 'application/x-ndjson' } }
  );
}
const open = () => bridge.open(1, identity);
const action = (purpose: 'save_as' | 'open' | 'reveal' | 'copy_path' = 'save_as') => ({
  ...grant,
  handleId: open().handleId,
  purpose,
  sourcePathTicket: 'b'.repeat(64),
});

beforeAll(async () => {
  await fs.mkdir(root, { recursive: true });
  directory = await fs.mkdtemp(path.join(root, 'run-'));
  source = path.join(directory, 'source.wav');
});
beforeEach(async () => {
  await fs.writeFile(source, bytes);
  lease = {
    kind: 'managed-local',
    acpUrl: 'ws://127.0.0.1:3210/acp',
    secretKey: 'backend-secret',
    cleanup: async () => undefined,
    windowIds: new Set([1]),
    cleanedUp: false,
    exited: false,
    exitCode: null,
    exitSignal: null,
  };
  cancelled = vi.fn<() => void>();
  deps = {
    getLease: () => lease,
    fetch: vi.fn((url, init) =>
      url.includes('/action-path/') ? Promise.resolve(heldResponse()) : content(url, init)
    ),
    chooseSavePath: vi.fn(async () => path.join(directory, 'saved.wav')),
    openPath: vi.fn(async () => ''),
    revealPath: vi.fn(),
    copyPath: vi.fn(),
  };
  bridge = new DesktopMediaBridge(deps);
});
afterEach(async () => {
  bridge.closeWindow(1);
  bridge.closeWindow(2);
  for (const name of await fs.readdir(directory)) await fs.unlink(path.join(directory, name));
  vi.useRealTimers();
});
afterAll(async () => {
  await fs.rmdir(directory);
});

describe('window-bound preview transport', () => {
  it('keeps backend secrets, tickets and registered paths out of opaque URLs', async () => {
    const handle = open();
    const preview = await bridge.preview(1, { ...grant, handleId: handle.handleId });
    expect(preview.url).toMatch(/^lumina-media:\/\/content\/[0-9a-f]{64}$/);
    expect(preview.url).not.toContain(grant.ticket);
    expect(preview.url).not.toContain(lease.secretKey);
    expect(bridge.authorizeProtocol(preview.url, 1)).toBe(true);
    expect(bridge.authorizeProtocol(preview.url, 2)).toBe(false);
    await expect(bridge.preview(2, { ...grant, handleId: handle.handleId })).rejects.toThrow();
    expect(() => bridge.close(2, handle.handleId)).toThrow();
    const call = vi.mocked(deps.fetch).mock.calls[0];
    expect(call[0]).toBe(
      `http://127.0.0.1:3210/media/v1/content/session-1/${identity.resultId}?purpose=preview`
    );
    expect(call[1]).toMatchObject({
      method: 'HEAD',
      redirect: 'error',
      credentials: 'omit',
      headers: { 'X-Secret-Key': 'backend-secret', 'X-Lumina-Media-Ticket': grant.ticket },
    });
    expect(call[1].headers).not.toHaveProperty('Origin');
  });
  it('revokes mappings when the lease changes or the window closes', async () => {
    const handle = open();
    const preview = await bridge.preview(1, { ...grant, handleId: handle.handleId });
    lease = { ...lease };
    expect(bridge.authorizeProtocol(preview.url, 1)).toBe(false);
    expect((await bridge.serve(new globalThis.Request(preview.url))).status).toBe(404);
    bridge.closeWindow(1);
    await expect(bridge.preview(1, { ...grant, handleId: handle.handleId })).rejects.toThrow();
  });
  it('renews the ticket while preserving the player URL and expires access', async () => {
    const handle = open();
    const first = await bridge.preview(1, { ...grant, handleId: handle.handleId });
    const second = await bridge.preview(1, {
      ...grant,
      ticket: 'c'.repeat(64),
      handleId: handle.handleId,
    });
    expect(first.url).toBe(second.url);
    const response = await bridge.serve(new globalThis.Request(second.url));
    expect(Array.from(new Uint8Array(await response.arrayBuffer()))).toEqual(Array.from(bytes));
    expect(vi.mocked(deps.fetch).mock.calls.slice(-1)[0]?.[1].headers).toMatchObject({
      'X-Lumina-Media-Ticket': 'c'.repeat(64),
    });
    vi.useFakeTimers();
    vi.advanceTimersByTime(300001);
    expect(bridge.authorizeProtocol(second.url, 1)).toBe(false);
  });
  it('streams single ranges and preserves 416 without forwarding backend headers', async () => {
    const preview = await bridge.preview(1, { ...grant, handleId: open().handleId });
    vi.mocked(deps.fetch).mockResolvedValueOnce(
      new Response(bytes.slice(1, 4), {
        status: 206,
        headers: {
          ...headers(),
          'Content-Length': '3',
          'Content-Range': `bytes 1-3/${bytes.length}`,
          'X-Private-Path': source,
        },
      })
    );
    const response = await bridge.serve(
      new globalThis.Request(preview.url, { headers: { Range: 'bytes=1-3' } })
    );
    expect(response.status).toBe(206);
    expect(response.headers.get('content-range')).toBe(`bytes 1-3/${bytes.length}`);
    expect(response.headers.has('X-Private-Path')).toBe(false);
    expect(Array.from(new Uint8Array(await response.arrayBuffer()))).toEqual(
      Array.from(bytes.slice(1, 4))
    );
    vi.mocked(deps.fetch).mockResolvedValueOnce(
      new Response(null, { status: 416, headers: { 'Content-Range': `bytes */${bytes.length}` } })
    );
    expect(
      (
        await bridge.serve(
          new globalThis.Request(preview.url, { headers: { Range: 'bytes=999-' } })
        )
      ).status
    ).toBe(416);
    const count = vi.mocked(deps.fetch).mock.calls.length;
    expect(
      (
        await bridge.serve(
          new globalThis.Request(preview.url, { headers: { Range: 'bytes=1-2,4-5' } })
        )
      ).status
    ).toBe(416);
    expect(vi.mocked(deps.fetch).mock.calls.length).toBe(count);
  });
  it.each([{ mime: 'text/html' }, { sha256: 'bad' }, { bytes: -1 }, { fileName: '../bad.wav' }])(
    'rejects malformed metadata before HTTP: %j',
    async (invalid) => {
      await expect(
        bridge.preview(1, {
          ...grant,
          metadata: { ...metadata, ...invalid },
          handleId: open().handleId,
        })
      ).rejects.toThrow();
      expect(deps.fetch).not.toHaveBeenCalled();
    }
  );
  it('rejects mismatched headers and full-body checksum/length corruption', async () => {
    const handle = open();
    vi.mocked(deps.fetch).mockResolvedValueOnce(
      new Response(null, { headers: { ...headers(), ETag: '"wrong"' } })
    );
    await expect(bridge.preview(1, { ...grant, handleId: handle.handleId })).rejects.toThrow();
    const preview = await bridge.preview(1, { ...grant, handleId: handle.handleId });
    for (const invalid of [bytes.slice(1), new Uint8Array(bytes.length)]) {
      vi.mocked(deps.fetch).mockResolvedValueOnce(new Response(invalid, { headers: headers() }));
      const response = await bridge.serve(new globalThis.Request(preview.url));
      await expect(response.arrayBuffer()).rejects.toThrow();
    }
  });
  it('cancels a pending preview reader immediately when its window closes', async () => {
    const preview = await bridge.preview(1, { ...grant, handleId: open().handleId });
    vi.mocked(deps.fetch).mockResolvedValueOnce(
      new Response(new ReadableStream({ cancel: cancelled }), { headers: headers() })
    );
    const response = await bridge.serve(new globalThis.Request(preview.url));
    const reading = response.arrayBuffer();
    bridge.closeWindow(1);
    await expect(reading).rejects.toThrow();
    expect(cancelled).toHaveBeenCalledOnce();
  });
  it('bounds handle allocation and rejects noncanonical logical identities', () => {
    expect(() => bridge.open(1, { ...identity, resultId: '../source' })).toThrow();
    expect(() => bridge.open(1, { ...identity, sessionId: 'a/b' })).toThrow();
    for (let index = 0; index < 64; index++) open();
    expect(open).toThrow();
  });
});

describe('native actions and verified save-as', () => {
  it.each(['open', 'reveal', 'copy_path', 'save_as'] as const)(
    'allows verified AVI files for %s independently of browser decoding',
    async (purpose) => {
      const avi = { ...metadata, fileName: 'output.avi', mime: 'video/x-msvideo' };
      vi.mocked(deps.fetch).mockImplementation(async (url) =>
        url.includes('/action-path/')
          ? heldResponse({ ...avi, path: source })
          : new Response(bytes, { headers: { ...headers(), 'Content-Type': avi.mime } })
      );
      expect(await bridge.action(1, { ...action(purpose), metadata: avi })).toEqual({
        status: 'completed',
      });
      expect(cancelled).toHaveBeenCalledOnce();
      if (purpose === 'save_as')
        expect(await fs.readFile(path.join(directory, 'saved.wav'))).toEqual(Buffer.from(bytes));
    }
  );
  it.each(['open', 'reveal', 'copy_path'] as const)(
    'holds the server guard through %s and releases it afterward',
    async (purpose) => {
      const checkHeld = () => {
        expect(cancelled).not.toHaveBeenCalled();
      };
      vi.mocked(deps.openPath).mockImplementation(async (file) => {
        expect(file).toBe(source);
        checkHeld();
        return '';
      });
      vi.mocked(deps.revealPath).mockImplementation(checkHeld);
      vi.mocked(deps.copyPath).mockImplementation(checkHeld);
      expect(await bridge.action(1, action(purpose))).toEqual({ status: 'completed' });
      expect(cancelled).toHaveBeenCalledOnce();
      expect(vi.mocked(deps.fetch).mock.calls[0][1].headers).toMatchObject({
        'X-Lumina-Media-Local-Action': '1',
      });
      expect(JSON.stringify(await bridge.action(1, action('copy_path')))).not.toContain(source);
    }
  );
  it('refuses native actions for an externally configured loopback backend', async () => {
    lease.kind = 'external';
    const handle = open();
    expect(handle.nativeActions).toBe(false);
    await expect(
      bridge.action(1, { ...grant, handleId: handle.handleId, purpose: 'open' })
    ).rejects.toThrow();
    expect(deps.fetch).not.toHaveBeenCalled();
  });
  it('keeps external save-as available and validates its content', async () => {
    lease.kind = 'external';
    expect(await bridge.action(1, action())).toEqual({ status: 'completed' });
    expect(await fs.readFile(path.join(directory, 'saved.wav'))).toEqual(Buffer.from(bytes));
    expect(vi.mocked(deps.fetch).mock.calls.every(([url]) => !url.includes('action-path'))).toBe(
      true
    );
  });
  it('saves through a temporary file and preserves the managed source', async () => {
    expect(await bridge.action(1, action())).toEqual({ status: 'completed' });
    expect(await fs.readFile(path.join(directory, 'saved.wav'))).toEqual(Buffer.from(bytes));
    expect(await fs.readFile(source)).toEqual(Buffer.from(bytes));
    expect(await fs.readdir(directory)).toEqual(['saved.wav', 'source.wav']);
    expect(cancelled).toHaveBeenCalledOnce();
  });
  it('treats dialog cancellation as cancellation without claiming HTTP or changing files', async () => {
    vi.mocked(deps.chooseSavePath).mockResolvedValue(null);
    expect(await bridge.action(1, action())).toEqual({ status: 'cancelled' });
    expect(deps.fetch).not.toHaveBeenCalled();
    expect(await fs.readdir(directory)).toEqual(['source.wav']);
  });
  it.each(['same', 'hardlink'] as const)(
    'rejects saving over the source through %s paths',
    async (variant) => {
      const target = variant === 'same' ? source : path.join(directory, 'alias.wav');
      if (variant === 'hardlink') await fs.link(source, target);
      vi.mocked(deps.chooseSavePath).mockResolvedValue(target);
      expect(await bridge.action(1, action())).toEqual({ status: 'failed' });
      expect(await fs.readFile(source)).toEqual(Buffer.from(bytes));
      expect(vi.mocked(deps.fetch).mock.calls).toHaveLength(1);
    }
  );
  it.each(['digest', 'truncated'] as const)(
    'preserves an existing destination and removes temporary data on %s failure',
    async (variant) => {
      const destination = path.join(directory, 'saved.wav');
      await fs.writeFile(destination, 'old-result');
      vi.mocked(deps.fetch).mockImplementation((url, init) =>
        url.includes('action-path')
          ? Promise.resolve(heldResponse())
          : Promise.resolve(
              new Response(
                init.method === 'HEAD'
                  ? null
                  : variant === 'digest'
                    ? new Uint8Array(bytes.length)
                    : bytes.slice(1),
                { headers: headers() }
              )
            )
      );
      expect(await bridge.action(1, action())).toEqual({ status: 'failed' });
      expect(await fs.readFile(destination, 'utf8')).toBe('old-result');
      expect(await fs.readdir(directory)).toEqual(['saved.wav', 'source.wav']);
    }
  );
  it('refuses to publish when the approved destination changes during streaming', async () => {
    const destination = path.join(directory, 'saved.wav');
    await fs.writeFile(destination, 'old');
    vi.mocked(deps.fetch).mockImplementation(async (url, init) => {
      if (url.includes('action-path')) return heldResponse();
      await fs.writeFile(destination, 'changed-during-copy');
      return content(url, init);
    });
    expect(await bridge.action(1, action())).toEqual({ status: 'failed' });
    expect(await fs.readFile(destination, 'utf8')).toBe('changed-during-copy');
  });
  it('rejects an ended guard or a shell failure and never returns a path', async () => {
    vi.mocked(deps.fetch).mockResolvedValueOnce(heldResponse(undefined, true));
    expect(await bridge.action(1, action('open'))).toEqual({ status: 'failed' });
    expect(deps.openPath).not.toHaveBeenCalled();
    vi.mocked(deps.openPath).mockResolvedValue('shell failed with private path');
    expect(await bridge.action(1, action('open'))).toEqual({ status: 'failed' });
  });
  it('aborts a pending save body on session/window close and leaves no partial output', async () => {
    let started!: () => void;
    const streaming = new Promise<void>((resolve) => {
      started = resolve;
    });
    vi.mocked(deps.fetch).mockImplementation(async (url) => {
      if (url.includes('action-path')) return heldResponse();
      started();
      return new Response(new ReadableStream({ cancel: cancelled }), { headers: headers() });
    });
    const saving = bridge.action(1, action());
    await streaming;
    bridge.closeWindow(1);
    expect(await saving).toEqual({ status: 'failed' });
    expect(await fs.readdir(directory)).toEqual(['source.wav']);
  });
});
