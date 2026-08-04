const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

const desktopDir = path.resolve(__dirname, '..');
const packageJson = require(path.join(desktopDir, 'package.json'));
const versionMatch = packageJson.version.match(/^(\d+)\.(\d+)\.(\d+)(?:-mcp\.(\d+))?$/);
if (!versionMatch) {
  throw new Error(`Unsupported Lumina version: ${packageJson.version}`);
}
const fileVersion = `${versionMatch[1]}.${versionMatch[2]}.${versionMatch[3]}.${versionMatch[4] || 0}`;
const appDirectory = path.join(desktopDir, 'out', 'Lumina-win32-x64');
const outputDirectory = path.join(desktopDir, 'out', 'make', 'inno');
const installerScript = path.join(desktopDir, 'installer', 'lumina.iss');
const installerName = `Lumina-${packageJson.version}-Windows-x64-Setup.exe`;
const requiredRuntimeFiles = [
  'Lumina.exe',
  path.join('resources', 'bin', 'goose.exe'),
  path.join('resources', 'bin', 'cublas64_12.dll'),
  path.join('resources', 'bin', 'cublasLt64_12.dll'),
  path.join('resources', 'bin', 'cudart64_12.dll'),
  path.join('resources', 'bin', 'curand64_10.dll'),
];

function findCompiler() {
  const candidates = [
    process.env.INNO_SETUP_COMPILER,
    process.env.LOCALAPPDATA &&
      path.join(process.env.LOCALAPPDATA, 'Programs', 'Inno Setup 6', 'ISCC.exe'),
    process.env['ProgramFiles(x86)'] &&
      path.join(process.env['ProgramFiles(x86)'], 'Inno Setup 6', 'ISCC.exe'),
    process.env.ProgramFiles && path.join(process.env.ProgramFiles, 'Inno Setup 6', 'ISCC.exe'),
  ].filter(Boolean);
  return candidates.find((candidate) => fs.existsSync(candidate));
}

if (process.platform !== 'win32') {
  throw new Error('The Lumina Windows installer must be built on Windows');
}

if (packageJson.productName !== 'Lumina') {
  throw new Error(`Expected productName Lumina, got ${packageJson.productName}`);
}

for (const relativePath of requiredRuntimeFiles) {
  const requiredPath = path.join(appDirectory, relativePath);
  if (!fs.existsSync(requiredPath)) {
    throw new Error(`Packaged runtime is incomplete: ${requiredPath}`);
  }
}

const compiler = findCompiler();
if (!compiler) {
  throw new Error('Inno Setup 6 compiler was not found; set INNO_SETUP_COMPILER');
}

fs.mkdirSync(outputDirectory, { recursive: true });
const result = spawnSync(
  compiler,
  [
    '/Qp',
    `/DMyAppVersion=${packageJson.version}`,
    `/DMyFileVersion=${fileVersion}`,
    `/DSourceDir=${appDirectory}`,
    `/DOutputDir=${outputDirectory}`,
    installerScript,
  ],
  { encoding: 'utf8', stdio: 'inherit' }
);

if (result.error) {
  throw result.error;
}
if (result.status !== 0) {
  throw new Error(`Inno Setup failed with exit code ${result.status}`);
}

const installerPath = path.join(outputDirectory, installerName);
if (!fs.existsSync(installerPath)) {
  throw new Error(`Inno Setup did not create ${installerPath}`);
}

const installerSize = fs.statSync(installerPath).size;
if (installerSize < 100 * 1024 * 1024) {
  throw new Error(`Installer is unexpectedly small: ${installerSize} bytes`);
}

console.log(`Created ${installerPath} (${installerSize} bytes)`);
