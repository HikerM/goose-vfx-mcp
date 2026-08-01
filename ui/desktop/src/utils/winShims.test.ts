import { beforeEach, describe, expect, it, vi } from 'vitest';

const fsMock = vi.hoisted(() => {
  let lockContents = '';
  return {
    access: vi.fn(async () => undefined),
    copyFile: vi.fn(async () => undefined),
    lstat: vi.fn(async () => ({ isSymbolicLink: () => false })),
    realpath: vi.fn(async (value: string) => value),
    mkdir: vi.fn(async () => undefined),
    rename: vi.fn(async () => undefined),
    rm: vi.fn(async () => undefined),
    open: vi.fn(async () => ({
      close: vi.fn(async () => undefined),
      writeFile: vi.fn(async (data: string) => {
        lockContents = data;
      }),
    })),
    writeFile: vi.fn(async (_path: string, data: string) => {
      lockContents = data;
    }),
    readFile: vi.fn(async () => lockContents),
  };
});

vi.mock('node:fs', () => ({ default: { promises: fsMock } }));
vi.mock('node:fs/promises', () => fsMock);
vi.mock('./logger', () => ({ default: { info: vi.fn(), error: vi.fn() } }));
vi.mock('./storageRootPolicy', async () => {
  const actual = await vi.importActual<typeof import('./storageRootPolicy')>('./storageRootPolicy');
  return { ...actual, preflightWindowsStorageDirectory: vi.fn(async () => undefined) };
});

import { ensureWinShims, windowsMcpSpawnEnvironment } from './winShims';
import { preflightWindowsStorageDirectory } from './storageRootPolicy';

describe('ensureWinShims', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    vi.clearAllMocks();
    process.env.PATH = 'C:\\Windows\\System32';
  });

  it('is a no-op outside Windows', async () => {
    vi.spyOn(process, 'platform', 'get').mockReturnValue('linux');
    await expect(ensureWinShims('D:\\Goose')).resolves.toBeUndefined();
    expect(fsMock.copyFile).not.toHaveBeenCalled();
    expect(process.env.PATH).toBe('C:\\Windows\\System32');
  });

  it('returns a child environment without changing the main process PATH', async () => {
    vi.spyOn(process, 'platform', 'get').mockReturnValue('win32');
    Object.defineProperty(process, 'resourcesPath', { configurable: true, value: 'C:\\Resources' });
    const originalPath = process.env.PATH;
    const result = await ensureWinShims('D:\\Goose');
    expect(fsMock.copyFile).toHaveBeenCalledTimes(4);
    expect(process.env.PATH).toBe(originalPath);
    expect(result?.env.PATH?.startsWith('D:\\Goose\\bin;')).toBe(true);
  });

  it('uses a governed custom D-drive root for the shim destination', async () => {
    vi.spyOn(process, 'platform', 'get').mockReturnValue('win32');
    Object.defineProperty(process, 'resourcesPath', { configurable: true, value: 'C:\\Resources' });

    await ensureWinShims('D:\\Other Goose');

    expect(fsMock.copyFile).toHaveBeenCalledWith(
      'C:\\Resources\\bin\\npx.cmd',
      expect.stringMatching(/D:\\Other Goose\\bin\.staging-.*\\npx\.cmd/)
    );
    expect(fsMock.copyFile).toHaveBeenCalledWith(
      'C:\\Resources\\bin\\npx-forward.ps1',
      expect.stringMatching(/D:\\Other Goose\\bin\.staging-.*\\npx-forward\.ps1/)
    );
    expect(process.env.PATH).toBe('C:\\Windows\\System32');
  });

  it.each(['C:\\Goose', '\\\\server\\share\\Goose', 'D:\\Other\\..\\Goose'])(
    'rejects a non-governed root: %s',
    async (root) => {
      vi.spyOn(process, 'platform', 'get').mockReturnValue('win32');

      await expect(ensureWinShims(root)).rejects.toThrow('local D drive');
      expect(fsMock.copyFile).not.toHaveBeenCalled();
    }
  );

  it('inserts the target when PATH contains only a similar directory', async () => {
    vi.spyOn(process, 'platform', 'get').mockReturnValue('win32');
    Object.defineProperty(process, 'resourcesPath', { configurable: true, value: 'C:\\Resources' });
    process.env.PATH = 'D:\\Goose\\bin-old;C:\\Windows\\System32';

    await ensureWinShims('D:\\Goose');

    expect(process.env.PATH).toBe('D:\\Goose\\bin-old;C:\\Windows\\System32');
  });

  it('does not duplicate a case-insensitively matching target', async () => {
    vi.spyOn(process, 'platform', 'get').mockReturnValue('win32');
    Object.defineProperty(process, 'resourcesPath', { configurable: true, value: 'C:\\Resources' });
    process.env.PATH = 'd:\\goose\\bin;C:\\Windows\\System32';

    await ensureWinShims('D:\\Goose');

    expect(process.env.PATH).toBe('d:\\goose\\bin;C:\\Windows\\System32');
  });

  it('moves an existing target from the middle to the beginning', async () => {
    vi.spyOn(process, 'platform', 'get').mockReturnValue('win32');
    Object.defineProperty(process, 'resourcesPath', { configurable: true, value: 'C:\\Resources' });
    process.env.PATH = 'C:\\Windows\\System32;D:\\Goose\\bin;C:\\Tools';

    await ensureWinShims('D:\\Goose');

    expect(process.env.PATH).toBe('C:\\Windows\\System32;D:\\Goose\\bin;C:\\Tools');
  });

  it.each(['copy', 'verify'])('%s failure rejects without changing PATH', async (failure) => {
    vi.spyOn(process, 'platform', 'get').mockReturnValue('win32');
    Object.defineProperty(process, 'resourcesPath', { configurable: true, value: 'C:\\Resources' });
    const originalPath = process.env.PATH;
    if (failure === 'copy') {
      fsMock.copyFile.mockRejectedValueOnce(new Error('copy failed'));
    } else {
      fsMock.realpath.mockResolvedValueOnce('D:\\Elsewhere');
    }

    await expect(ensureWinShims('D:\\Goose')).rejects.toThrow('required Windows shims');
    expect(process.env.PATH).toBe(originalPath);
    expect(preflightWindowsStorageDirectory).toHaveBeenCalled();
  });

  it('clones PATH and removes duplicate shim entries', () => {
    const base = { PATH: 'C:\\Tools;D:\\Goose\\bin;C:\\Other' };
    const child = windowsMcpSpawnEnvironment(base, 'd:\\goose\\bin');
    expect(child).toEqual({ PATH: 'd:\\goose\\bin;C:\\Tools;C:\\Other' });
    expect(base.PATH).toBe('C:\\Tools;D:\\Goose\\bin;C:\\Other');
  });

  it('uses a unique staging directory while holding an exclusive deployment lock', async () => {
    vi.spyOn(process, 'platform', 'get').mockReturnValue('win32');
    Object.defineProperty(process, 'resourcesPath', { configurable: true, value: 'C:\\Resources' });

    await ensureWinShims('D:\\Goose');

    expect(fsMock.open).toHaveBeenCalledWith('D:\\Goose\\bin.deploy.lock', 'wx');
    const copiedDestinations = fsMock.copyFile.mock.calls.map(
      (call) => (call as unknown as [string, string])[1]
    );
    expect(copiedDestinations.every((value) => /\.staging-[0-9a-f]{24}\\/.test(value))).toBe(true);
  });

  it('fails closed when another process keeps the deployment lock', async () => {
    vi.spyOn(process, 'platform', 'get').mockReturnValue('win32');
    fsMock.open.mockRejectedValue(new Object(Object.assign(new Error('busy'), { code: 'EEXIST' })));

    await expect(ensureWinShims('D:\\Goose')).rejects.toThrow('deployment lock');
    expect(fsMock.copyFile).not.toHaveBeenCalled();
  }, 7000);
});
