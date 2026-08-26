import { createRequire } from "node:module";
import { dirname, join } from "node:path";

const PLATFORMS: Record<string, string> = {
  "darwin-arm64": "@hikerm/lumina-binary-darwin-arm64",
  "darwin-x64": "@hikerm/lumina-binary-darwin-x64",
  "linux-arm64": "@hikerm/lumina-binary-linux-arm64",
  "linux-x64": "@hikerm/lumina-binary-linux-x64",
  "win32-x64": "@hikerm/lumina-binary-win32-x64",
};

/**
 * Resolves the path to the lumina binary.
 *
 * Resolution order:
 *   1. `LUMINA_BINARY` environment variable (explicit override)
 *   2. Platform-specific `@hikerm/lumina-binary-*` optional dependency
 *
 * @throws if no binary can be found
 */
export function resolveLuminaBinary(): string {
  const envBinary = process.env.LUMINA_BINARY;
  if (envBinary) return envBinary;

  const key = `${process.platform}-${process.arch}`;
  const pkg = PLATFORMS[key];
  if (!pkg) {
    throw new Error(
      `No lumina binary available for ${key}. Set LUMINA_BINARY to the path of a lumina binary.`,
    );
  }

  try {
    const require = createRequire(import.meta.url);
    const pkgDir = dirname(require.resolve(`${pkg}/package.json`));
    const binName = process.platform === "win32" ? "lumina.exe" : "lumina";
    return join(pkgDir, "bin", binName);
  } catch {
    throw new Error(
      `lumina binary package ${pkg} is not installed. Set LUMINA_BINARY or install the native package.`,
    );
  }
}
