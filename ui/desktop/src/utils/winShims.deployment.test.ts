import { describe, expect, it, vi } from 'vitest';

vi.mock('./logger', () => ({ default: { info: vi.fn(), error: vi.fn() } }));

import { ensureWinShims } from './winShims';

describe.skipIf(process.platform !== 'win32')('real Windows shim deployment', () => {
  it('publishes the authored npx pair under D:\\Goose\\bin', async () => {
    Object.defineProperty(process, 'resourcesPath', {
      configurable: true,
      value: new URL('../platform/windows', import.meta.url).pathname.replace(/^\//, '').replace(/\//g, '\\'),
    });
    const result = await ensureWinShims('D:\\Goose');
    expect(result?.shimDirectory.toLowerCase()).toBe('d:\\goose\\bin');
    expect(result?.env.PATH?.toLowerCase()).toContain('d:\\goose\\bin');
  });
});
