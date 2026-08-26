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
    "lumina-cli",
    "--bin",
    "lumina",
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
  const luminaBinary = resolve(targetDir, "lumina.exe");
  const binDir = resolve(desktopRoot, "src", "bin");

  if (!existsSync(luminaBinary)) {
    throw new Error(`Backend binary not found at ${luminaBinary}`);
  }

  mkdirSync(binDir, { recursive: true });
  copyFileSync(luminaBinary, resolve(binDir, "lumina.exe"));

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
