const { execFileSync, execSync } = require("child_process");
const { copyFileSync, existsSync, mkdirSync, readdirSync } = require("fs");
const { resolve, join } = require("path");

const desktopRoot = resolve(__dirname, "..");
const repoRoot = resolve(desktopRoot, "..", "..");

function buildWindowsBackend() {
  const cargoCommand = "powershell.exe";
  const cargoArgs = [
    "-NoLogo",
    "-NoProfile",
    "-ExecutionPolicy",
    "Bypass",
    "-File",
    resolve(repoRoot, "bin", "cargo.ps1"),
    "build",
    "--release",
    "-p",
    "goose-cli",
    "--bin",
    "goose",
  ];

  execFileSync(
    cargoCommand,
    cargoArgs,
    {
      cwd: repoRoot,
      stdio: "inherit",
    },
  );

  const targetDir = resolve(repoRoot, "target", "release");
  const gooseBinary = resolve(targetDir, "goose.exe");
  const binDir = resolve(desktopRoot, "src", "bin");

  if (!existsSync(gooseBinary)) {
    throw new Error(`Backend binary not found at ${gooseBinary}`);
  }

  mkdirSync(binDir, { recursive: true });
  copyFileSync(gooseBinary, resolve(binDir, "goose.exe"));

  for (const entry of readdirSync(targetDir)) {
    if (!entry.toLowerCase().endsWith(".dll")) {
      continue;
    }

    copyFileSync(resolve(targetDir, entry), join(binDir, entry));
  }
}

if (process.platform === "win32") {
  buildWindowsBackend();
  execFileSync("pnpm.cmd", ["run", "start-gui"], {
    cwd: desktopRoot,
    stdio: "inherit",
    env: process.env,
  });
} else {
  execSync("just run-ui", {
    cwd: repoRoot,
    stdio: "inherit",
    env: process.env,
  });
}
