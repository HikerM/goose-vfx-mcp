import { describe, expect, it, vi } from 'vitest';
import {
  createStorageReadyGate,
  deriveDesktopUserDataPath,
  deriveWindowsShimsPath,
  preflightWindowsStorageDirectory,
  preflightWindowsStorageRoot,
  resolveDesktopGoosePathRoot,
  runAfterStorageReady,
  isPathWithinRoot,
  resolveManagedPath,
  isAllowedManagedFilePath,
  isAllowedRecipeDefaultName,
  isTrustedAppUrl,
} from '../storageRootPolicy';
import type { WindowsStoragePreflightFileSystem } from '../storageRootPolicy';

function preflightFs(existing: string[] = []): WindowsStoragePreflightFileSystem {
  const paths = new Set(existing.map((value) => value.toLowerCase()));
  const mkdir = vi.fn(async (value: string) => {
    const segments = value.split('\\');
    for (let index = 2; index <= segments.length; index += 1) {
      paths.add(`${segments.slice(0, index).join('\\')}`.toLowerCase());
    }
    return undefined;
  });
  const lstat = vi.fn(async (value: string) => {
      if (!paths.has(value.toLowerCase())) {
        const error = Object.assign(new Error('missing'), { code: 'ENOENT' });
        throw error;
      }
      return { isDirectory: () => true, isSymbolicLink: () => false } as never;
    });
  const realpath = vi.fn(async (value: string) => value);
  return {
    lstat,
    realpath,
    mkdir,
    access: vi.fn(async () => undefined),
  };
}

describe('resolveDesktopGoosePathRoot', () => {
  it('only permits nested .goosehints files and rejects sensitive managed paths', () => {
    expect(isAllowedManagedFilePath('Projects/demo/.goosehints')).toBe(true);
    expect(isAllowedManagedFilePath('.goosehints')).toBe(false);
    expect(isAllowedManagedFilePath('settings/.goosehints')).toBe(false);
    expect(isAllowedManagedFilePath('bin/tool.exe')).toBe(false);
    expect(isAllowedManagedFilePath('../outside/.goosehints')).toBe(false);
    expect(isAllowedManagedFilePath('Projects/demo/config.json')).toBe(false);
  });

  it('accepts only safe recipe file names as dialog defaults', () => {
    expect(isAllowedRecipeDefaultName('recipe.yaml')).toBe(true);
    expect(isAllowedRecipeDefaultName('my-recipe.yml')).toBe(true);
    expect(isAllowedRecipeDefaultName('..\\secret.yaml')).toBe(false);
    expect(isAllowedRecipeDefaultName('recipe.yaml\\other')).toBe(false);
    expect(isAllowedRecipeDefaultName('recipe.txt')).toBe(false);
  });

  it('trusts only the configured app origin and packaged app file', () => {
    expect(isTrustedAppUrl('http://localhost:5173/#/settings', 'http://localhost:5173/')).toBe(true);
    expect(isTrustedAppUrl('http://evil.test/#/settings', 'http://localhost:5173/')).toBe(false);
    expect(isTrustedAppUrl('file:///app/index.html#/settings', 'file:///app/index.html')).toBe(true);
    expect(isTrustedAppUrl('file:///other.html', 'file:///app/index.html')).toBe(false);
  });

  it('uses relative path boundaries instead of string prefixes', () => {
    expect(isPathWithinRoot('/managed/goose', '/managed/goose/config/file')).toBe(true);
    expect(isPathWithinRoot('/managed/goose', '/managed/goose-copy/file')).toBe(false);
    expect(isPathWithinRoot('/managed/goose', '/managed/goose/../outside')).toBe(false);
    expect(() => resolveManagedPath('/managed/goose', '../outside')).toThrow();
    expect(() => resolveManagedPath('/managed/goose', 'C:\\outside')).toThrow();
    expect(isPathWithinRoot('D:\\Goose', 'd:\\goose\\config\\file')).toBe(true);
    expect(isPathWithinRoot('D:\\Goose', 'D:\\Goose-copy\\file')).toBe(false);
    expect(isPathWithinRoot('D:\\Goose', '\\\\server\\share\\file')).toBe(false);
  });
  it('does not run a gated operation before storage is ready', async () => {
    let release!: () => void;
    const storageReady = new Promise<void>((resolve) => {
      release = resolve;
    });
    const operation = vi.fn(() => 'ready');
    const result = runAfterStorageReady(storageReady, operation);

    expect(operation).not.toHaveBeenCalled();
    release();
    await expect(result).resolves.toBe('ready');
    expect(operation).toHaveBeenCalledOnce();
  });

  it('converges a rejected storage promise once without rejecting the gated operation', async () => {
    const storageError = new Error('storage unavailable');
    const storageReady = Promise.reject(storageError);
    const onFailure = vi.fn();
    const gate = createStorageReadyGate(storageReady, onFailure);
    const operation = vi.fn(() => 'must not run');

    await expect(gate(operation)).resolves.toBeUndefined();
    await expect(gate(operation)).resolves.toBeUndefined();

    expect(operation).not.toHaveBeenCalled();
    expect(onFailure).toHaveBeenCalledOnce();
    expect(onFailure).toHaveBeenCalledWith(storageError);
  });

  it('falls back to the governed D root for missing or blank Windows configuration', () => {
    expect(resolveDesktopGoosePathRoot(undefined, 'win32')).toBe('D:\\Goose');
    expect(resolveDesktopGoosePathRoot('  ', 'win32')).toBe('D:\\Goose');
  });

  it('accepts explicit D subdirectories on Windows', () => {
    expect(resolveDesktopGoosePathRoot('d:/Goose Team/storage', 'win32')).toBe(
      'd:\\Goose Team\\storage'
    );
  });

  it('rejects C drive, relative, UNC, and bare drive roots on Windows', () => {
    for (const candidate of ['C:\\Goose', 'Goose', '\\\\server\\share\\goose', 'D:\\']) {
      expect(() => resolveDesktopGoosePathRoot(candidate, 'win32')).toThrow(/local D drive/i);
    }
  });

  it('keeps non-Windows behavior unchanged', () => {
    expect(resolveDesktopGoosePathRoot('/tmp/goose', 'linux')).toBe('/tmp/goose');
    expect(resolveDesktopGoosePathRoot(undefined, 'linux')).toBeUndefined();
  });

  it('derives Desktop user data and shim paths from the validated root', () => {
    const root = resolveDesktopGoosePathRoot('D:\\Goose Team\\storage', 'win32')!;
    expect(deriveDesktopUserDataPath(root, 'win32')).toBe('D:\\Goose Team\\storage\\desktop');
    expect(deriveWindowsShimsPath(root, 'win32')).toBe('D:\\Goose Team\\storage\\bin');
  });

  it('preflights the governed desktop and bin children', async () => {
    const fileSystem = preflightFs(['D:\\', 'D:\\Goose']);
    await preflightWindowsStorageDirectory('D:\\Goose\\desktop', fileSystem);
    await preflightWindowsStorageDirectory('D:\\Goose\\bin', fileSystem);
    expect(fileSystem.mkdir).toHaveBeenCalledWith('D:\\Goose\\desktop', { recursive: true });
    expect(fileSystem.mkdir).toHaveBeenCalledWith('D:\\Goose\\bin', { recursive: true });
  });

  it('rejects an unsafe desktop or bin child before creating it', async () => {
    for (const child of ['desktop', 'bin']) {
      const fileSystem = preflightFs(['D:\\', 'D:\\Goose']);
      const lstat = vi.mocked(fileSystem.lstat);
      lstat.mockImplementation(async (value: string) => {
        if (value.toLowerCase() === `d:\\goose\\${child}`) {
          return { isDirectory: () => true, isSymbolicLink: () => true } as never;
        }
        if (
          value.toLowerCase() === 'd:\\goose' ||
          !value.toLowerCase().startsWith('d:\\goose')
        ) {
          return { isDirectory: () => true, isSymbolicLink: () => false } as never;
        }
        const error = Object.assign(new Error('missing'), { code: 'ENOENT' });
        throw error;
      });
      await expect(
        preflightWindowsStorageDirectory(`D:\\Goose\\${child}`, fileSystem)
      ).rejects.toThrow(/local D drive/i);
      expect(fileSystem.mkdir).not.toHaveBeenCalled();
    }
  });

  it('rejects a non-directory child before creating it', async () => {
    const fileSystem = preflightFs(['D:\\', 'D:\\Goose']);
    const lstat = vi.mocked(fileSystem.lstat);
    lstat.mockImplementation(async (value: string) => {
      if (value.toLowerCase() === 'd:\\goose\\bin') {
        return { isDirectory: () => false, isSymbolicLink: () => false } as never;
      }
      return { isDirectory: () => true, isSymbolicLink: () => false } as never;
    });

    await expect(
      preflightWindowsStorageDirectory('D:\\Goose\\bin', fileSystem)
    ).rejects.toThrow(/local D drive/i);
    expect(fileSystem.mkdir).not.toHaveBeenCalled();
  });

  it('rejects a child whose real path differs before creating it', async () => {
    const fileSystem = preflightFs(['D:\\', 'D:\\Goose', 'D:\\Goose\\desktop']);
    const realpath = vi.mocked(fileSystem.realpath);
    realpath.mockImplementation(async (value: string) =>
      value.toLowerCase() === 'd:\\goose\\desktop' ? 'D:\\Redirected' : value
    );

    await expect(
      preflightWindowsStorageDirectory('D:\\Goose\\desktop', fileSystem)
    ).rejects.toThrow(/local D drive/i);
    expect(fileSystem.mkdir).not.toHaveBeenCalled();
  });

  it('preflights an existing directory chain without creating it', async () => {
    const fileSystem = preflightFs(['D:\\', 'D:\\Goose']);
    await preflightWindowsStorageRoot('D:\\Goose', fileSystem);
    expect(fileSystem.mkdir).not.toHaveBeenCalled();
    expect(fileSystem.access).toHaveBeenCalledWith('D:\\Goose', expect.any(Number));
  });

  it('creates a missing root only after its existing ancestor passes inspection', async () => {
    const fileSystem = preflightFs(['D:\\']);
    await preflightWindowsStorageRoot('D:\\Goose', fileSystem);
    expect(fileSystem.mkdir).toHaveBeenCalledWith('D:\\Goose', { recursive: true });
  });

  it('creates nested missing parents from the nearest verified ancestor', async () => {
    const fileSystem = preflightFs(['D:\\']);
    await preflightWindowsStorageRoot('D:\\Goose Team\\storage', fileSystem);
    expect(fileSystem.mkdir).toHaveBeenCalledWith('D:\\Goose Team\\storage', {
      recursive: true,
    });
  });

  it('rejects a symlinked directory before attempting mkdir', async () => {
    const fileSystem = preflightFs(['D:\\']);
    const lstat = vi.mocked(fileSystem.lstat);
    lstat.mockImplementationOnce(async () => ({
      isDirectory: () => true,
      isSymbolicLink: () => true,
    }) as never);
    await expect(preflightWindowsStorageRoot('D:\\Goose', fileSystem)).rejects.toThrow(
      /local D drive/i
    );
    expect(fileSystem.mkdir).not.toHaveBeenCalled();
  });

  it('rejects a directory whose real path differs before attempting mkdir', async () => {
    const fileSystem = preflightFs(['D:\\', 'D:\\Goose']);
    const realpath = vi.mocked(fileSystem.realpath);
    realpath.mockResolvedValueOnce('D:\\Other');
    await expect(preflightWindowsStorageRoot('D:\\Goose', fileSystem)).rejects.toThrow(
      /local D drive/i
    );
    expect(fileSystem.mkdir).not.toHaveBeenCalled();
  });
});
