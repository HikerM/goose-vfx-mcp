import { expandTilde } from './pathUtils';
import fs from 'node:fs/promises';
import { constants as fsConstants } from 'node:fs';
import type { Stats } from 'node:fs';
import path from 'node:path';

export const DEFAULT_WINDOWS_GOOSE_PATH_ROOT = 'D:\\Goose';
export const WINDOWS_DESKTOP_DATA_DIRECTORY = 'desktop';
export const WINDOWS_SHIMS_DIRECTORY = 'bin';

function pathModuleFor(root: string, candidate: string) {
  return /^[a-z]:[\\/]/i.test(root) || /^[a-z]:[\\/]/i.test(candidate) || root.startsWith('\\\\')
    ? path.win32
    : path;
}

export function isPathWithinRoot(root: string, candidate: string): boolean {
  const pathApi = pathModuleFor(root, candidate);
  const relative = pathApi.relative(pathApi.resolve(root), pathApi.resolve(candidate));
  return (
    relative === '' ||
    (relative !== '..' && !relative.startsWith(`..${pathApi.sep}`) && !pathApi.isAbsolute(relative))
  );
}

export function resolveManagedPath(root: string, relativePath: string): string {
  const pathApi = pathModuleFor(root, relativePath);
  if (!relativePath || pathApi.isAbsolute(relativePath) || path.win32.isAbsolute(relativePath)) {
    throw new Error('Managed paths must be relative to the Lumina storage root');
  }
  const candidate = pathApi.resolve(root, relativePath);
  if (!isPathWithinRoot(root, candidate)) {
    throw new Error('Path is outside the Lumina storage root');
  }
  return candidate;
}

const MANAGED_FILE_NAME = /^\.goosehints$/i;
const FORBIDDEN_MANAGED_SEGMENTS = new Set([
  'settings',
  'credentials',
  'credential',
  'secret',
  'secrets',
  'token',
  'tokens',
  'bin',
  'runtime',
  'runtimes',
  'desktop',
]);

export function isAllowedManagedFilePath(relativePath: string): boolean {
  if (typeof relativePath !== 'string' || relativePath.length === 0 || relativePath.length > 512) {
    return false;
  }
  if (/^(?:[a-z]:[\\/]|[\\/]{1,2})/i.test(relativePath)) return false;
  const segments = relativePath.replace(/\\/g, '/').split('/');
  return (
    segments.length > 1 &&
    segments[segments.length - 1] !== undefined &&
    MANAGED_FILE_NAME.test(segments[segments.length - 1]) &&
    segments.every(
      (segment) =>
        segment.length > 0 &&
        segment !== '.' &&
        segment !== '..' &&
        !segment.includes(':') &&
        !FORBIDDEN_MANAGED_SEGMENTS.has(segment.toLowerCase())
    )
  );
}

export function isAllowedRecipeDefaultName(value: unknown): value is string {
  if (typeof value !== 'string' || value.includes('\0') || value.length > 128) return false;
  const name = path.basename(value).toLowerCase();
  return name === value.toLowerCase() && /^(?:[a-z0-9][a-z0-9._-]*)?\.(?:yaml|yml)$/.test(name);
}

export function isTrustedAppUrl(actualUrl: string, expectedUrl: string): boolean {
  try {
    const actual = new URL(actualUrl);
    const expected = new URL(expectedUrl);
    return (
      actual.protocol === expected.protocol &&
      actual.host === expected.host &&
      (actual.protocol !== 'file:' || actual.pathname === expected.pathname)
    );
  } catch {
    return false;
  }
}

export async function assertManagedPath(root: string, candidate: string): Promise<void> {
  if (!isPathWithinRoot(root, candidate)) {
    throw new Error('Path is outside the Lumina storage root');
  }

  const resolvedRoot = await fs.realpath(root);
  let current = path.resolve(root);
  const relative = path.relative(current, path.resolve(candidate));
  for (const segment of relative ? relative.split(path.sep) : []) {
    current = path.join(current, segment);
    try {
      const stats = await fs.lstat(current);
      if (stats.isSymbolicLink()) {
        throw new Error('Managed paths cannot traverse symbolic links');
      }
      const resolvedCurrent = await fs.realpath(current);
      if (!isPathWithinRoot(resolvedRoot, resolvedCurrent)) {
        throw new Error('Managed paths cannot leave the Lumina storage root');
      }
    } catch (error) {
      if ((error as { code?: string }).code === 'ENOENT') break;
      throw error;
    }
  }
}

const WINDOWS_STORAGE_ROOT_ERROR =
  'Lumina can only use a regular folder on the local D drive for managed MCP storage on Windows. Choose a folder on D and restart Lumina.';

export function resolveDesktopGoosePathRoot(
  envPathRoot: string | undefined = process.env.GOOSE_PATH_ROOT,
  platform: typeof process.platform = process.platform
): string | undefined {
  const trimmed = envPathRoot?.trim();
  if (platform !== 'win32') {
    return trimmed ? expandTilde(trimmed) : undefined;
  }
  if (!trimmed) {
    return DEFAULT_WINDOWS_GOOSE_PATH_ROOT;
  }

  const expanded = expandTilde(trimmed).replace(/\//g, '\\');
  if (!isSafeWindowsDPath(expanded)) {
    throw new Error(WINDOWS_STORAGE_ROOT_ERROR);
  }
  return expanded;
}

export interface WindowsStoragePreflightFileSystem {
  lstat(path: string): Promise<Stats>;
  realpath(path: string): Promise<string>;
  mkdir(path: string, options: { recursive: true }): Promise<string | undefined>;
  access(path: string, mode?: number): Promise<void>;
}

export type StorageReadyFailureHandler = (error: unknown) => void | Promise<void>;

export function createStorageReadyGate(
  storageReady: Promise<void>,
  onFailure: StorageReadyFailureHandler
): <T>(operation: () => T | Promise<T>) => Promise<T | undefined> {
  let failureHandled = false;

  const handleFailure = (error: unknown): void => {
    if (failureHandled) return;
    failureHandled = true;
    void Promise.resolve()
      .then(() => onFailure(error))
      .catch(() => undefined);
  };

  const settledStorageReady = storageReady.then(
    () => true,
    (error) => {
      handleFailure(error);
      return false;
    }
  );

  return <T>(operation: () => T | Promise<T>): Promise<T | undefined> =>
    settledStorageReady.then((ready) => (ready ? operation() : undefined));
}

export function runAfterStorageReady<T>(
  storageReady: Promise<void>,
  operation: () => T | Promise<T>,
  onFailure: StorageReadyFailureHandler = () => undefined
): Promise<T | undefined> {
  return createStorageReadyGate(storageReady, onFailure)(operation);
}

function isMissingPath(error: unknown): boolean {
  return typeof error === 'object' && error !== null && 'code' in error && error.code === 'ENOENT';
}

function normalizeWindowsPath(value: string): string {
  return value
    .replace(/^\\\\\?\\/, '')
    .replace(/\//g, '\\')
    .replace(/[\\]+$/, '')
    .toLowerCase();
}

async function verifyWindowsStorageDirectoryChain(
  root: string,
  fileSystem: WindowsStoragePreflightFileSystem
): Promise<void> {
  const segments = root.length > 3 ? root.slice(3).split('\\') : [];
  let current = 'D:\\';

  await verifyWindowsStorageDirectory(current, fileSystem);

  for (const segment of segments) {
    current = `${current}${segment}`;
    await verifyWindowsStorageDirectory(current, fileSystem);
    current += '\\';
  }
}

async function verifyWindowsStorageDirectory(
  current: string,
  fileSystem: WindowsStoragePreflightFileSystem
): Promise<void> {
  const stats = await fileSystem.lstat(current);
  if (!stats.isDirectory() || stats.isSymbolicLink()) {
    throw new Error(WINDOWS_STORAGE_ROOT_ERROR);
  }

  const resolved = await fileSystem.realpath(current);
  if (normalizeWindowsPath(resolved) !== normalizeWindowsPath(current)) {
    throw new Error(WINDOWS_STORAGE_ROOT_ERROR);
  }
}

async function findNearestExistingWindowsAncestor(
  root: string,
  fileSystem: WindowsStoragePreflightFileSystem
): Promise<string> {
  const segments = root.slice(3).split('\\');
  let current = 'D:\\';
  await verifyWindowsStorageDirectory(current, fileSystem);

  for (const segment of segments) {
    const candidate = `${current}${segment}`;
    try {
      await verifyWindowsStorageDirectory(candidate, fileSystem);
      current = `${candidate}\\`;
    } catch (error) {
      if (isMissingPath(error)) {
        return current;
      }
      throw error;
    }
  }

  return current;
}

export async function preflightWindowsStorageDirectory(
  root: string,
  fileSystem: WindowsStoragePreflightFileSystem = fs
): Promise<void> {
  if (!isSafeWindowsDPath(root)) {
    throw new Error(WINDOWS_STORAGE_ROOT_ERROR);
  }

  let rootExists = true;
  try {
    await verifyWindowsStorageDirectoryChain(root, fileSystem);
  } catch (error) {
    if (isMissingPath(error)) {
      rootExists = false;
    } else {
      throw error;
    }
  }

  if (!rootExists) {
    await findNearestExistingWindowsAncestor(root, fileSystem);
    await fileSystem.mkdir(root, { recursive: true });
    await verifyWindowsStorageDirectoryChain(root, fileSystem);
  }

  await fileSystem.access(root, fsConstants.W_OK);
}

export async function preflightWindowsStorageRoot(
  root: string,
  fileSystem: WindowsStoragePreflightFileSystem = fs
): Promise<void> {
  await preflightWindowsStorageDirectory(root, fileSystem);
}

export function deriveDesktopUserDataPath(
  root: string,
  platform: typeof process.platform = process.platform
): string {
  if (platform !== 'win32') {
    return `${root}/${WINDOWS_DESKTOP_DATA_DIRECTORY}`;
  }
  return `${root}\\${WINDOWS_DESKTOP_DATA_DIRECTORY}`;
}

export function deriveWindowsShimsPath(
  root: string,
  platform: typeof process.platform = process.platform
): string {
  if (platform !== 'win32') {
    return `${root}/${WINDOWS_SHIMS_DIRECTORY}`;
  }
  return `${root}\\${WINDOWS_SHIMS_DIRECTORY}`;
}

function isSafeWindowsDPath(value: string): boolean {
  if (value.startsWith('\\\\') || value.startsWith('//')) {
    return false;
  }
  if (value.length < 4 || !/^d:\\/i.test(value)) {
    return false;
  }

  const remainder = value.slice(3);
  if (!remainder || remainder.includes(':')) {
    return false;
  }

  const segments = remainder.split('\\');
  return segments.every(
    (segment) =>
      segment.length > 0 &&
      segment !== '.' &&
      segment !== '..' &&
      !segment.endsWith(' ') &&
      !segment.endsWith('.')
  );
}
