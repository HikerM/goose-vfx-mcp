import fs from 'node:fs';
import crypto from 'node:crypto';
import path from 'node:path';
import log from './logger';
import {
  deriveWindowsShimsPath,
  preflightWindowsStorageDirectory,
  resolveDesktopGoosePathRoot,
  type WindowsStoragePreflightFileSystem,
} from './storageRootPolicy';

function isMissingPath(error: unknown): boolean {
  return typeof error === 'object' && error !== null && 'code' in error && error.code === 'ENOENT';
}

function normalizeWindowsPath(value: string): string {
  return path.win32.normalize(value).replace(/[\\]+$/, '').toLowerCase();
}

async function verifyShimDestination(
  destination: string,
  fileSystem: Pick<WindowsStoragePreflightFileSystem, 'lstat' | 'realpath'>
): Promise<void> {
  const stats = await fileSystem.lstat(destination).catch((error: unknown) => {
    if (isMissingPath(error)) return undefined;
    throw error;
  });
  if (!stats) return;
  if (stats.isSymbolicLink()) throw new Error('unsafe destination');
  const resolved = await fileSystem.realpath(destination);
  if (normalizeWindowsPath(resolved) !== normalizeWindowsPath(destination)) {
    throw new Error('unsafe destination');
  }
}

async function removeOwnedDirectory(target: string, ownerRoot: string, token: string): Promise<void> {
  const item = await fs.promises.lstat(target).catch((error: unknown) => (isMissingPath(error) ? undefined : Promise.reject(error)));
  if (!item || !item.isDirectory() || item.isSymbolicLink()) return;
  const resolved = normalizeWindowsPath(await fs.promises.realpath(target));
  const root = normalizeWindowsPath(ownerRoot);
  if (resolved !== normalizeWindowsPath(target) || !resolved.startsWith(`${root}\\`)) return;
  const marker = path.join(target, '.owner-token');
  const observed = await fs.promises.readFile(marker, 'utf8').catch(() => undefined);
  if (observed !== token) return;
  const again = await fs.promises.realpath(target);
  if (normalizeWindowsPath(again) !== resolved) return;
  await fs.promises.rm(target, { recursive: true, force: false });
}

async function acquireDeploymentLock(lockPath: string): Promise<fs.promises.FileHandle> {
  const deadline = Date.now() + 5000;
  const token = crypto.randomBytes(24).toString('hex');
  while (true) {
    try {
      const handle = await fs.promises.open(lockPath, 'wx');
      await handle.writeFile(token, 'utf8');
      const stat = await fs.promises.lstat(lockPath);
      const resolved = await fs.promises.realpath(lockPath);
      if (stat.isSymbolicLink() || normalizeWindowsPath(resolved) !== normalizeWindowsPath(lockPath)) {
        await handle.close();
        throw new Error('unsafe deployment lock');
      }
      const observed = await fs.promises.readFile(lockPath, 'utf8');
      if (observed !== token) { await handle.close(); throw new Error('deployment lock owner mismatch'); }
      (handle as fs.promises.FileHandle & { ownerToken?: string }).ownerToken = token;
      return handle;
    } catch (error: unknown) {
      if ((error as NodeJS.ErrnoException).code !== 'EEXIST' || Date.now() >= deadline) {
        throw new Error('Windows shim deployment lock is unavailable');
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }
}

/**
 * Ensures Windows shims are available under the governed Goose storage root.
 * This allows the bundled executables to be found via PATH regardless of where Goose is installed
 */
export interface WindowsMcpSpawnEnvironment {
  env: NodeJS.ProcessEnv;
  shimDirectory: string;
}

export async function ensureWinShims(root: string): Promise<WindowsMcpSpawnEnvironment | undefined> {
  if (process.platform !== 'win32') return undefined;

  const governedRoot = resolveDesktopGoosePathRoot(root, 'win32');
  if (!governedRoot) throw new Error('Failed to prepare required Windows shims');

  const srcDir = path.join(process.resourcesPath, 'bin'); // existing dir
  const tgtDir = deriveWindowsShimsPath(governedRoot, process.platform);

  try {
    await preflightWindowsStorageDirectory(tgtDir);
    const lockPath = `${tgtDir}.deploy.lock`;
    const lock = await acquireDeploymentLock(lockPath);
    const suffix = crypto.randomBytes(12).toString('hex');
    const stagingDir = `${tgtDir}.staging-${suffix}`;
    const previousDir = `${tgtDir}.previous-${suffix}`;
    const requiredShims = ['npx.cmd', 'npx-forward.ps1'];
    const optionalShims = ['uvx.exe', 'uv.exe'];
    const shims = [
      ...requiredShims,
      ...(await Promise.all(
        optionalShims.map(async (shim) => {
          try {
            await fs.promises.access(path.join(srcDir, shim));
            return shim;
          } catch {
            return undefined;
          }
        })
      )).filter((shim): shim is string => shim !== undefined),
    ];

    try {
      await preflightWindowsStorageDirectory(lockPath);
      await fs.promises.mkdir(stagingDir, { recursive: true });
      await verifyShimDestination(stagingDir, fs.promises);
      await fs.promises.writeFile(path.join(stagingDir, '.owner-token'), suffix, { encoding: 'utf8', flag: 'wx' });

    await Promise.all(
      shims.map(async (shim) => {
        const src = path.join(srcDir, shim);
        const dst = path.join(stagingDir, shim);
        await fs.promises.access(src);
        await fs.promises.copyFile(src, dst);
        await verifyShimDestination(dst, fs.promises);
        log.info(`Copied Windows shim: ${shim}`);
      })
    );

      await fs.promises.rename(tgtDir, previousDir).catch((error: unknown) => {
      if (!isMissingPath(error)) throw error;
    });
      try {
        await fs.promises.rename(stagingDir, tgtDir);
      } catch (error) {
        await fs.promises.rename(previousDir, tgtDir).catch(() => undefined);
        throw error;
      }

      return { env: windowsMcpSpawnEnvironment(process.env, tgtDir), shimDirectory: tgtDir };
    } finally {
      await lock.close();
      const token = (lock as fs.promises.FileHandle & { ownerToken?: string }).ownerToken;
      await removeOwnedDirectory(previousDir, governedRoot, suffix).catch(() => undefined);
      await removeOwnedDirectory(stagingDir, governedRoot, suffix).catch(() => undefined);
      if (token) {
        const lockStat = await fs.promises.lstat(lockPath).catch(() => undefined);
        const lockOwner = await fs.promises.readFile(lockPath, 'utf8').catch(() => undefined);
        if (lockStat && !lockStat.isSymbolicLink() && lockOwner === token) await fs.promises.rm(lockPath, { force: false });
      }
    }
  } catch (error) {
    log.error('Failed to ensure required Windows shims', error);
    throw new Error(
      `Failed to prepare required Windows shims: ${error instanceof Error ? error.message : String(error)}`
    );
  }
}

export function windowsMcpSpawnEnvironment(
  baseEnvironment: NodeJS.ProcessEnv = process.env,
  shimDirectory: string
): NodeJS.ProcessEnv {
  const currentPath = baseEnvironment.PATH ?? '';
  const parts = currentPath.split(path.delimiter).filter(
    (part) => normalizeWindowsPath(part) !== normalizeWindowsPath(shimDirectory)
  );
  return { ...baseEnvironment, PATH: `${shimDirectory}${path.delimiter}${parts.join(path.delimiter)}` };
}
