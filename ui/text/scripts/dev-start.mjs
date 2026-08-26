#!/usr/bin/env node

// Development entrypoint: ensures a lumina binary is available, then launches the TUI
// Skips the cargo build if LUMINA_BINARY is already set or if --server is provided

import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(__dirname, "..", "..", "..");
const args = process.argv.slice(2);
const hasServerFlag = args.some(
  (arg) =>
    arg === "--server" ||
    arg === "-s" ||
    arg.startsWith("--server=") ||
    arg.startsWith("-s="),
);

function getCargoCommand(cargoArgs) {
  if (process.platform !== "win32") {
    return {
      command: "cargo",
      args: cargoArgs,
    };
  }

  return {
    command: "powershell.exe",
    args: [
      "-NoLogo",
      "-NoProfile",
      "-ExecutionPolicy",
      "Bypass",
      "-File",
      join(repoRoot, "bin", "cargo.ps1"),
      ...cargoArgs,
    ],
  };
}

if (!hasServerFlag && !process.env.LUMINA_BINARY) {
  const binName = process.platform === "win32" ? "lumina.exe" : "lumina";
  const binaryPath = join(repoRoot, "target", "debug", binName);
  const cargoCommand = getCargoCommand(["build", "-p", "lumina-cli"]);

  console.log("Building lumina (debug)…");
  execFileSync(cargoCommand.command, cargoCommand.args, {
    cwd: repoRoot,
    stdio: "inherit",
  });

  if (!existsSync(binaryPath)) {
    console.error(`Build succeeded but binary not found at ${binaryPath}`);
    process.exit(1);
  }

  process.env.LUMINA_BINARY = binaryPath;
}

execFileSync("tsx", [join(__dirname, "..", "src", "tui.tsx"), ...process.argv.slice(2)], {
  cwd: process.cwd(),
  stdio: "inherit",
  env: process.env,
});
