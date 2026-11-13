import { createHash, randomBytes } from 'node:crypto';
import fs from 'node:fs/promises';
import path from 'node:path';
import { httpBaseFromAcpWebSocketUrl, isLoopbackAcpWebSocketUrl } from './acp/url';
import type { LuminaServeLease } from './luminaServeLeaseRegistry';
import type { MediaActionOutcome, MediaResultIdentity } from './components/media/types';
import type {
  DesktopMediaActionRequest,
  DesktopMediaGrant,
  DesktopMediaHandle,
  DesktopMediaPreviewRequest,
  DesktopMediaPurpose,
  MediaContentMetadata,
} from './types/desktopMedia';

export interface DesktopMediaBridgeDependencies {
  getLease(windowId: number): LuminaServeLease | null;
  fetch(url: string, init: globalThis.RequestInit): Promise<Response>;
  chooseSavePath(windowId: number, fileName: string): Promise<string | null>;
  openPath(filePath: string): Promise<string>;
  revealPath(filePath: string): void;
  copyPath(filePath: string): void;
}

type Handle = MediaResultIdentity & {
  id: string;
  url: string;
  windowId: number;
  lease: LuminaServeLease;
  controllers: Set<AbortController>;
  actionPending: boolean;
  preview?: { grant: DesktopMediaGrant; expiresAt: number };
};
type PathRecord = MediaContentMetadata & { path: string };
type HeldPath = { value: PathRecord; alive(): boolean; close(): Promise<void> };

const mimeTypes = new Set([
  'image/png',
  'image/jpeg',
  'image/webp',
  'image/gif',
  'image/avif',
  'audio/wav',
  'audio/wave',
  'audio/x-wav',
  'audio/mpeg',
  'audio/mp3',
  'audio/ogg',
  'audio/webm',
  'audio/mp4',
  'audio/aac',
  'audio/flac',
  'audio/x-flac',
  'video/mp4',
  'video/webm',
  'video/ogg',
  'video/x-msvideo',
]);
const nativePurposes = new Set<DesktopMediaPurpose>(['open', 'reveal', 'copy_path']);
const MAX_HANDLES_PER_WINDOW = 64;
const MAX_REQUESTS_PER_HANDLE = 6;

function fail(): never {
  throw new Error('Media access is unavailable.');
}
function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
function hasControlCharacters(value: string): boolean {
  return Array.from(value).some((character) => character.charCodeAt(0) < 32);
}
function validIdentity(value: unknown): value is MediaResultIdentity {
  return (
    record(value) &&
    typeof value.sessionId === 'string' &&
    value.sessionId.length > 0 &&
    value.sessionId.length <= 256 &&
    !/[\\/]/.test(value.sessionId) &&
    !hasControlCharacters(value.sessionId) &&
    typeof value.resultId === 'string' &&
    /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value.resultId)
  );
}
function validTicket(ticket: unknown): ticket is string {
  return typeof ticket === 'string' && /^[a-zA-Z0-9_-]{16,512}$/.test(ticket);
}
function validMetadata(value: unknown): value is MediaContentMetadata {
  return (
    record(value) &&
    typeof value.fileName === 'string' &&
    value.fileName.length > 0 &&
    value.fileName.length <= 255 &&
    !/[\\/]/.test(value.fileName) &&
    !hasControlCharacters(value.fileName) &&
    typeof value.mime === 'string' &&
    mimeTypes.has(value.mime) &&
    Number.isSafeInteger(value.bytes) &&
    Number(value.bytes) > 0 &&
    typeof value.sha256 === 'string' &&
    /^[0-9a-f]{64}$/.test(value.sha256)
  );
}
function validateGrant(value: unknown): asserts value is DesktopMediaGrant {
  if (
    !record(value) ||
    !validTicket(value.ticket) ||
    !validMetadata(value.metadata) ||
    !Number.isSafeInteger(value.expiresInMs) ||
    Number(value.expiresInMs) <= 0 ||
    Number(value.expiresInMs) > 300000
  )
    fail();
}
function samePath(left: string, right: string): boolean {
  const normalize = (value: string) => (process.platform === 'win32' ? value.toLowerCase() : value);
  return normalize(path.resolve(left)) === normalize(path.resolve(right));
}
function nativePath(value: string): string {
  if (process.platform !== 'win32') return value;
  const ordinary = /^\\\\\?\\[a-zA-Z]:\\/.test(value) ? value.slice(4) : value;
  if (!/^[a-zA-Z]:\\/.test(ordinary)) fail();
  return ordinary;
}
async function destinationState(filePath: string) {
  try {
    const stat = await fs.lstat(filePath);
    if (!stat.isFile() || stat.isSymbolicLink()) fail();
    return stat;
  } catch (error) {
    if ((error as { code?: string }).code === 'ENOENT') return null;
    throw error;
  }
}
function sameIdentity(
  left: { dev: number; ino: number },
  right: { dev: number; ino: number }
): boolean {
  return left.dev === right.dev && left.ino === right.ino;
}

export class DesktopMediaBridge {
  private readonly handles = new Map<string, Handle>();
  constructor(private readonly dependencies: DesktopMediaBridgeDependencies) {}

  open(windowId: number, identity: MediaResultIdentity): DesktopMediaHandle {
    if (!validIdentity(identity)) fail();
    const lease = this.dependencies.getLease(windowId);
    if (!lease || lease.exited || lease.cleanedUp) fail();
    if (
      [...this.handles.values()].filter((item) => item.windowId === windowId).length >=
      MAX_HANDLES_PER_WINDOW
    )
      fail();
    const id = randomBytes(32).toString('hex');
    this.handles.set(id, {
      id,
      url: `lumina-media://content/${randomBytes(32).toString('hex')}`,
      windowId,
      lease,
      sessionId: identity.sessionId,
      resultId: identity.resultId,
      controllers: new Set(),
      actionPending: false,
    });
    return { handleId: id, nativeActions: lease.kind === 'managed-local' };
  }

  private current(handle: Handle): boolean {
    return (
      this.handles.get(handle.id) === handle &&
      this.dependencies.getLease(handle.windowId) === handle.lease &&
      !handle.lease.exited &&
      !handle.lease.cleanedUp
    );
  }

  private get(windowId: number, id: string): Handle {
    const handle = this.handles.get(id);
    if (!handle || handle.windowId !== windowId || !this.current(handle)) fail();
    return handle;
  }

  close(windowId: number, id: string): void {
    const handle = this.handles.get(id);
    if (!handle) return;
    if (handle.windowId !== windowId) fail();
    this.handles.delete(id);
    for (const controller of handle.controllers) controller.abort();
    handle.controllers.clear();
  }

  closeWindow(windowId: number): void {
    for (const handle of [...this.handles.values()]) {
      if (handle.windowId === windowId) this.close(windowId, handle.id);
    }
  }

  authorizeProtocol(url: string, windowId: number): boolean {
    const handle = [...this.handles.values()].find((item) => item.url === url);
    return (
      !!handle &&
      handle.windowId === windowId &&
      this.current(handle) &&
      !!handle.preview &&
      handle.preview.expiresAt > Date.now()
    );
  }

  private async request(
    handle: Handle,
    route: 'content' | 'action-path',
    purpose: DesktopMediaPurpose,
    ticket: string,
    method = 'GET',
    range?: string
  ) {
    if (
      !this.current(handle) ||
      !validTicket(ticket) ||
      handle.controllers.size >= MAX_REQUESTS_PER_HANDLE
    )
      fail();
    const url = new URL(
      `${httpBaseFromAcpWebSocketUrl(handle.lease.acpUrl)}/media/v1/${route}/${encodeURIComponent(handle.sessionId)}/${encodeURIComponent(handle.resultId)}`
    );
    url.searchParams.set('purpose', purpose);
    const controller = new AbortController();
    handle.controllers.add(controller);
    const timer = setTimeout(() => controller.abort(), 15000);
    const headers: Record<string, string> = {
      'X-Secret-Key': handle.lease.secretKey,
      'X-Lumina-Media-Ticket': ticket,
    };
    if (route === 'action-path') headers['X-Lumina-Media-Local-Action'] = '1';
    if (range !== undefined) headers.Range = range;
    try {
      const response = await this.dependencies.fetch(url.toString(), {
        method,
        headers,
        signal: controller.signal,
        redirect: 'error',
        cache: 'no-store',
        credentials: 'omit',
      });
      if (!this.current(handle) || controller.signal.aborted) {
        await response.body?.cancel();
        fail();
      }
      return {
        response,
        controller,
        finish: () => {
          controller.abort();
          handle.controllers.delete(controller);
        },
      };
    } catch (error) {
      controller.abort();
      handle.controllers.delete(controller);
      throw error;
    } finally {
      clearTimeout(timer);
    }
  }

  private contentHeaders(response: Response, expected: MediaContentMetadata): number {
    if (
      response.headers.get('content-type')?.split(';', 1)[0].trim() !== expected.mime ||
      response.headers.get('etag') !== `"${expected.sha256}"` ||
      response.headers.has('content-encoding')
    )
      fail();
    const rawLength = response.headers.get('content-length');
    if (!rawLength || !/^\d+$/.test(rawLength)) fail();
    const length = Number(rawLength);
    if (!Number.isSafeInteger(length) || length < 0 || length > expected.bytes) fail();
    if (response.status === 200 && length !== expected.bytes) fail();
    if (response.status === 206) {
      const match = response.headers.get('content-range')?.match(/^bytes (\d+)-(\d+)\/(\d+)$/);
      if (
        !match ||
        Number(match[3]) !== expected.bytes ||
        Number(match[2]) < Number(match[1]) ||
        Number(match[2]) >= expected.bytes ||
        Number(match[2]) - Number(match[1]) + 1 !== length
      )
        fail();
    }
    return length;
  }

  async preview(windowId: number, request: DesktopMediaPreviewRequest): Promise<{ url: string }> {
    validateGrant(request);
    const handle = this.get(windowId, request.handleId);
    const probe = await this.request(handle, 'content', 'preview', request.ticket, 'HEAD');
    try {
      if (probe.response.status !== 200) fail();
      this.contentHeaders(probe.response, request.metadata);
      if (!this.current(handle)) fail();
      handle.preview = {
        grant: {
          ticket: request.ticket,
          expiresInMs: request.expiresInMs,
          metadata: { ...request.metadata },
        },
        expiresAt: Date.now() + request.expiresInMs,
      };
      return { url: handle.url };
    } finally {
      await probe.response.body?.cancel();
      probe.finish();
    }
  }

  async serve(request: globalThis.Request): Promise<Response> {
    const handle = [...this.handles.values()].find((item) => item.url === request.url);
    if (
      !handle ||
      !this.current(handle) ||
      !handle.preview ||
      handle.preview.expiresAt <= Date.now()
    )
      return new Response(null, { status: 404 });
    if (request.method !== 'GET' && request.method !== 'HEAD')
      return new Response(null, { status: 405 });
    const range = request.headers.get('range') ?? undefined;
    if (range !== undefined && (!/^bytes=(?:\d+-\d*|-\d+)$/.test(range) || range.length > 100))
      return new Response(null, { status: 416 });
    const grant = handle.preview.grant;
    let upstream: Awaited<ReturnType<DesktopMediaBridge['request']>> | undefined;
    try {
      upstream = await this.request(
        handle,
        'content',
        'preview',
        grant.ticket,
        request.method,
        range
      );
      const { response, controller, finish } = upstream;
      if (response.status === 416) {
        if (response.headers.get('content-range') !== `bytes */${grant.metadata.bytes}`) fail();
        await response.body?.cancel();
        finish();
        return new Response(null, {
          status: 416,
          headers: {
            'Content-Range': `bytes */${grant.metadata.bytes}`,
            'Cache-Control': 'no-store',
          },
        });
      }
      if (response.status !== 200 && response.status !== 206) fail();
      if (range && response.status !== 206) fail();
      const length = this.contentHeaders(response, grant.metadata);
      const headers = new globalThis.Headers({
        'Content-Type': grant.metadata.mime,
        'Content-Length': String(length),
        'Accept-Ranges': 'bytes',
        ETag: `"${grant.metadata.sha256}"`,
        'Cache-Control': 'no-store',
        'X-Content-Type-Options': 'nosniff',
        'Content-Security-Policy': "default-src 'none'; sandbox",
      });
      if (response.status === 206)
        headers.set('Content-Range', response.headers.get('content-range')!);
      if (request.method === 'HEAD') {
        await response.body?.cancel();
        finish();
        return new Response(null, { status: response.status, headers });
      }
      if (!response.body) fail();
      const reader = response.body.getReader();
      let readBytes = 0;
      const hash = response.status === 200 ? createHash('sha256') : null;
      const onAbort = () => {
        controller.abort();
        void reader.cancel().catch(() => undefined);
      };
      const onUpstreamAbort = () => {
        void reader.cancel().catch(() => undefined);
      };
      controller.signal.addEventListener('abort', onUpstreamAbort, { once: true });
      request.signal.addEventListener('abort', onAbort, { once: true });
      if (request.signal.aborted) onAbort();
      const cleanup = () => {
        request.signal.removeEventListener('abort', onAbort);
        controller.signal.removeEventListener('abort', onUpstreamAbort);
        finish();
      };
      const body = new ReadableStream<Uint8Array>({
        async pull(output) {
          try {
            if (controller.signal.aborted) fail();
            const next = await reader.read();
            if (next.done) {
              if (readBytes !== length || (hash && hash.digest('hex') !== grant.metadata.sha256))
                fail();
              cleanup();
              output.close();
              return;
            }
            readBytes += next.value.byteLength;
            if (readBytes > length) fail();
            hash?.update(next.value);
            output.enqueue(next.value);
          } catch (error) {
            await reader.cancel().catch(() => undefined);
            cleanup();
            output.error(error);
          }
        },
        async cancel() {
          await reader.cancel().catch(() => undefined);
          cleanup();
        },
      });
      return new Response(body, { status: response.status, headers });
    } catch {
      await upstream?.response.body?.cancel().catch(() => undefined);
      upstream?.finish();
      return new Response(null, { status: 404 });
    }
  }

  private async heldPath(
    handle: Handle,
    purpose: DesktopMediaPurpose,
    ticket: string,
    metadata: MediaContentMetadata
  ): Promise<HeldPath> {
    if (handle.lease.kind !== 'managed-local' || !isLoopbackAcpWebSocketUrl(handle.lease.acpUrl))
      fail();
    const upstream = await this.request(handle, 'action-path', purpose, ticket);
    const reader = upstream.response.body?.getReader();
    let closed = false;
    let ended = false;
    const onAbort = () => {
      ended = true;
      void reader?.cancel().catch(() => undefined);
    };
    upstream.controller.signal.addEventListener('abort', onAbort, { once: true });
    const firstLineTimer = setTimeout(() => upstream.controller.abort(), 15000);
    const close = async () => {
      closed = true;
      clearTimeout(firstLineTimer);
      upstream.controller.signal.removeEventListener('abort', onAbort);
      await reader?.cancel().catch(() => undefined);
      upstream.finish();
    };
    try {
      if (
        upstream.response.status !== 200 ||
        upstream.response.headers.get('content-type')?.split(';', 1)[0] !==
          'application/x-ndjson' ||
        !reader
      )
        fail();
      let prefix = Buffer.alloc(0);
      while (!prefix.includes(10)) {
        const next = await reader.read();
        if (next.done) fail();
        prefix = Buffer.concat([prefix, next.value]);
        if (prefix.length > 16384) fail();
      }
      clearTimeout(firstLineTimer);
      const value: unknown = JSON.parse(prefix.subarray(0, prefix.indexOf(10)).toString('utf8'));
      if (
        !record(value) ||
        !validMetadata(value) ||
        typeof value.path !== 'string' ||
        !path.isAbsolute(value.path) ||
        hasControlCharacters(value.path)
      )
        fail();
      if (
        value.sha256 !== metadata.sha256 ||
        value.bytes !== metadata.bytes ||
        value.mime !== metadata.mime ||
        value.fileName !== metadata.fileName
      )
        fail();
      void (async () => {
        try {
          while (!closed) {
            if ((await reader.read()).done) break;
          }
        } catch {
          /* The action must fail if the guard stream closes before it finishes. */
        }
        if (!closed) ended = true;
      })();
      return {
        value: { ...value, path: nativePath(value.path) } as PathRecord,
        alive: () =>
          !ended && !closed && !upstream.controller.signal.aborted && this.current(handle),
        close,
      };
    } catch (error) {
      await close();
      throw error;
    }
  }

  async action(windowId: number, request: DesktopMediaActionRequest): Promise<MediaActionOutcome> {
    validateGrant(request);
    const handle = this.get(windowId, request.handleId);
    if (
      handle.actionPending ||
      (!nativePurposes.has(request.purpose) && request.purpose !== 'save_as')
    )
      fail();
    if (nativePurposes.has(request.purpose) && handle.lease.kind !== 'managed-local') fail();
    handle.actionPending = true;
    let held: HeldPath | undefined;
    try {
      if (request.purpose === 'save_as') {
        const chosen = await this.dependencies.chooseSavePath(windowId, request.metadata.fileName);
        if (!chosen) return { status: 'cancelled' };
        if (!this.current(handle)) fail();
        if (handle.lease.kind === 'managed-local') {
          if (!validTicket(request.sourcePathTicket)) fail();
          held = await this.heldPath(
            handle,
            'copy_path',
            request.sourcePathTicket,
            request.metadata
          );
        }
        await this.save(handle, request, chosen, held);
      } else {
        held = await this.heldPath(handle, request.purpose, request.ticket, request.metadata);
        if (!held.alive()) fail();
        if (request.purpose === 'open') {
          if (await this.dependencies.openPath(held.value.path)) fail();
        } else if (request.purpose === 'reveal') {
          this.dependencies.revealPath(held.value.path);
        } else {
          this.dependencies.copyPath(held.value.path);
        }
        if (!held.alive()) fail();
      }
      return { status: 'completed' };
    } catch {
      return { status: 'failed' };
    } finally {
      await held?.close();
      handle.actionPending = false;
    }
  }

  private async save(
    handle: Handle,
    grant: DesktopMediaGrant,
    chosen: string,
    source?: HeldPath
  ): Promise<void> {
    if (!path.isAbsolute(chosen) || hasControlCharacters(chosen)) fail();
    const parent = await fs.realpath(path.dirname(chosen));
    const parentIdentity = await fs.stat(parent);
    const destination = path.join(parent, path.basename(chosen));
    const before = await destinationState(destination);
    if (source) {
      const canonicalSource = await fs.realpath(source.value.path);
      if (
        samePath(canonicalSource, destination) ||
        (before && sameIdentity(before, await fs.stat(canonicalSource)))
      )
        fail();
    }
    const temporary = path.join(parent, `.lumina-save-${randomBytes(32).toString('hex')}.tmp`);
    let file: Awaited<ReturnType<typeof fs.open>> | undefined;
    let upstream: Awaited<ReturnType<DesktopMediaBridge['request']>> | undefined;
    let reader: globalThis.ReadableStreamDefaultReader<Uint8Array> | undefined;
    let published = false;
    const onAbort = () => {
      void reader?.cancel().catch(() => undefined);
    };
    try {
      upstream = await this.request(handle, 'content', 'save_as', grant.ticket);
      if (upstream.response.status !== 200) fail();
      this.contentHeaders(upstream.response, grant.metadata);
      reader = upstream.response.body?.getReader();
      if (!reader) fail();
      upstream.controller.signal.addEventListener('abort', onAbort, { once: true });
      file = await fs.open(temporary, 'wx', 0o600);
      const digest = createHash('sha256');
      let bytes = 0;
      while (true) {
        if (
          !this.current(handle) ||
          upstream.controller.signal.aborted ||
          (source && !source.alive())
        )
          fail();
        const idleTimer = setTimeout(() => upstream?.controller.abort(), 30000);
        const chunk = await reader.read().finally(() => clearTimeout(idleTimer));
        if (chunk.done) break;
        bytes += chunk.value.byteLength;
        if (bytes > grant.metadata.bytes) fail();
        digest.update(chunk.value);
        let offset = 0;
        while (offset < chunk.value.byteLength) {
          const written = await file.write(chunk.value, offset, chunk.value.byteLength - offset);
          if (written.bytesWritten === 0) fail();
          offset += written.bytesWritten;
        }
      }
      if (bytes !== grant.metadata.bytes || digest.digest('hex') !== grant.metadata.sha256) fail();
      await file.sync();
      await file.close();
      file = undefined;
      const after = await destinationState(destination);
      if (
        !!before !== !!after ||
        (before &&
          after &&
          (!sameIdentity(before, after) ||
            before.size !== after.size ||
            before.mtimeMs !== after.mtimeMs))
      )
        fail();
      if (
        !samePath(await fs.realpath(path.dirname(chosen)), parent) ||
        !sameIdentity(parentIdentity, await fs.stat(parent))
      )
        fail();
      if (!this.current(handle) || (source && !source.alive())) fail();
      await fs.rename(temporary, destination);
      published = true;
    } finally {
      await reader?.cancel().catch(() => undefined);
      await file?.close().catch(() => undefined);
      upstream?.controller.signal.removeEventListener('abort', onAbort);
      upstream?.finish();
      if (!published) await fs.unlink(temporary).catch(() => undefined);
    }
  }
}
