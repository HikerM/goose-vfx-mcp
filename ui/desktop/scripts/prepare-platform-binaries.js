const fs = require('fs');
const crypto = require('crypto');
const https = require('https');
const os = require('os');
const path = require('path');
const { execFileSync } = require('child_process');

// Paths
const srcBinDir = path.join(__dirname, '..', 'src', 'bin');
const platformWinDir = path.join(__dirname, '..', 'src', 'platform', 'windows', 'bin');
const repositoryRoot = path.resolve(__dirname, '..', '..', '..');
const backendBinaryPath = path.join(srcBinDir, 'lumina.exe');
const backendBuildStampPath = path.join(srcBinDir, 'lumina-backend-build.json');
const acpSchemaPath = path.join(repositoryRoot, 'crates', 'lumina', 'acp-schema.json');
const uvVersion = '0.11.11';
const uvDownloadUrl = `https://github.com/astral-sh/uv/releases/download/${uvVersion}/uv-x86_64-pc-windows-msvc.zip`;
const uvBinaryHashes = {
  'uv.exe': 'b1645e948603c12dd741987d0c072471195e18dd299b42334477ceac694f0af8',
  'uvx.exe': '0305c488dc29c16df1483c02a902d21a6798b0744f8e9eb34271d6b3e4bf6e2a',
};
const windowsCudaRuntimeFiles = [
  'cublas64_12.dll',
  'cublasLt64_12.dll',
  'cudart64_12.dll',
  'curand64_10.dll',
];
const allowedWindowsExecutables = new Set(['lumina.exe', 'uv.exe', 'uvx.exe']);
const backendInputExtensions = new Set(['.json', '.rs', '.toml', '.yaml', '.yml']);

// Platform-specific file patterns
const windowsFiles = ['*.exe', '*.dll', '*.cmd', 'lumina-npm/**/*'];

// Helper function to check if file matches patterns
function matchesPattern(filename, patterns) {
  return patterns.some((pattern) => {
    if (pattern.includes('**')) {
      // Handle directory patterns
      const basePattern = pattern.split('/**')[0];
      return filename.startsWith(basePattern);
    } else if (pattern.includes('*')) {
      // Handle wildcard patterns - be more precise with file extensions
      if (pattern.startsWith('*.')) {
        // For file extension patterns like *.exe, *.dll
        const extension = pattern.substring(2); // Remove "*."
        return filename.endsWith('.' + extension);
      } else {
        // For other wildcard patterns
        const regex = new RegExp('^' + pattern.replace(/\*/g, '.*') + '$');
        return regex.test(filename);
      }
    } else {
      // Exact match
      return filename === pattern;
    }
  });
}

function sha256(filePath) {
  const hash = crypto.createHash('sha256');
  hash.update(fs.readFileSync(filePath));
  return hash.digest('hex');
}

function hasExpectedHash(filePath, expectedHash) {
  return fs.existsSync(filePath) && sha256(filePath) === expectedHash;
}

function collectBackendInputFiles(targetPath, collected) {
  const stat = fs.statSync(targetPath);
  if (stat.isFile()) {
    if (backendInputExtensions.has(path.extname(targetPath).toLowerCase())) {
      collected.push(targetPath);
    }
    return;
  }

  for (const entry of fs.readdirSync(targetPath, { withFileTypes: true })) {
    if (entry.name === 'target') continue;
    collectBackendInputFiles(path.join(targetPath, entry.name), collected);
  }
}

function backendInputsSha256() {
  const files = [path.join(repositoryRoot, 'Cargo.toml'), path.join(repositoryRoot, 'Cargo.lock')];
  for (const root of ['crates', 'vendor']) {
    collectBackendInputFiles(path.join(repositoryRoot, root), files);
  }
  files.sort((left, right) => left.localeCompare(right, 'en'));

  const hash = crypto.createHash('sha256');
  for (const filePath of files) {
    const relativePath = path.relative(repositoryRoot, filePath).replaceAll('\\', '/');
    hash.update(relativePath);
    hash.update('\0');
    hash.update(fs.readFileSync(filePath));
    hash.update('\0');
  }
  return hash.digest('hex');
}

function expectedBackendBuildStamp() {
  if (!fs.existsSync(backendBinaryPath)) {
    throw new Error(`Lumina backend binary is missing: ${backendBinaryPath}`);
  }
  if (!fs.existsSync(acpSchemaPath)) {
    throw new Error(`Lumina ACP schema is missing: ${acpSchemaPath}`);
  }

  const packageVersion = require('../package.json').version;
  return {
    schemaVersion: 1,
    product: 'Lumina',
    packageVersion,
    binarySha256: sha256(backendBinaryPath),
    backendInputsSha256: backendInputsSha256(),
    acpSchemaSha256: sha256(acpSchemaPath),
  };
}

function writeOrValidateBackendBuildStamp() {
  const expected = expectedBackendBuildStamp();
  if (process.env.LUMINA_DESKTOP_WRITE_BACKEND_STAMP === '1') {
    fs.writeFileSync(backendBuildStampPath, `${JSON.stringify(expected, null, 2)}\n`, 'utf8');
    console.log('Wrote Lumina backend build stamp');
    return;
  }

  if (!fs.existsSync(backendBuildStampPath)) {
    throw new Error(
      'Lumina backend build stamp is missing. Run scripts/build-windows.ps1 before packaging.'
    );
  }
  const actual = JSON.parse(fs.readFileSync(backendBuildStampPath, 'utf8'));
  for (const [key, value] of Object.entries(expected)) {
    if (actual[key] !== value) {
      throw new Error(
        `Lumina backend build stamp mismatch for ${key}. Rebuild the backend before packaging.`
      );
    }
  }
  console.log('Verified Lumina backend binary and ACP schema are current');
}

function downloadFile(url, destPath, redirectsRemaining = 5) {
  return new Promise((resolve, reject) => {
    https
      .get(url, (response) => {
        if (
          response.statusCode >= 300 &&
          response.statusCode < 400 &&
          response.headers.location &&
          redirectsRemaining > 0
        ) {
          response.resume();
          downloadFile(response.headers.location, destPath, redirectsRemaining - 1)
            .then(resolve)
            .catch(reject);
          return;
        }

        if (response.statusCode !== 200) {
          response.resume();
          reject(new Error(`Failed to download ${url}: HTTP ${response.statusCode}`));
          return;
        }

        const file = fs.createWriteStream(destPath);
        response.pipe(file);
        file.on('finish', () => file.close(resolve));
        file.on('error', reject);
      })
      .on('error', reject);
  });
}

function extractZip(zipPath, destDir) {
  if (process.platform === 'win32') {
    execFileSync(
      'powershell.exe',
      [
        '-NoProfile',
        '-ExecutionPolicy',
        'Bypass',
        '-Command',
        `Expand-Archive -LiteralPath '${zipPath.replace(/'/g, "''")}' -DestinationPath '${destDir.replace(/'/g, "''")}' -Force`,
      ],
      { stdio: 'inherit' }
    );
    return;
  }

  execFileSync('unzip', ['-q', zipPath, '-d', destDir], { stdio: 'inherit' });
}

async function ensureWindowsUvBinaries() {
  const allPresent = Object.entries(uvBinaryHashes).every(([name, expectedHash]) =>
    hasExpectedHash(path.join(srcBinDir, name), expectedHash)
  );

  if (allPresent) {
    console.log(`Pinned uv ${uvVersion} binaries already present`);
    return;
  }

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lumina-uv-'));
  const zipPath = path.join(tmpDir, 'uv.zip');
  const extractDir = path.join(tmpDir, 'extract');
  fs.mkdirSync(extractDir, { recursive: true });

  try {
    console.log(`Downloading uv ${uvVersion} from ${uvDownloadUrl}`);
    await downloadFile(uvDownloadUrl, zipPath);
    extractZip(zipPath, extractDir);

    for (const [name, expectedHash] of Object.entries(uvBinaryHashes)) {
      const extractedPath = path.join(extractDir, name);
      if (!fs.existsSync(extractedPath)) {
        throw new Error(`Downloaded uv archive did not contain ${name}`);
      }

      const actualHash = sha256(extractedPath);
      if (actualHash !== expectedHash) {
        throw new Error(
          `${name} checksum mismatch for uv ${uvVersion}: expected ${expectedHash}, got ${actualHash}`
        );
      }

      fs.copyFileSync(extractedPath, path.join(srcBinDir, name));
      console.log(`Copied pinned ${name}`);
    }
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
}

function syncWindowsCudaRuntimeBinaries() {
  const includeCuda = process.env.LUMINA_DESKTOP_CUDA === '1';

  if (!includeCuda) {
    for (const name of windowsCudaRuntimeFiles) {
      const destination = path.join(srcBinDir, name);
      if (fs.existsSync(destination)) {
        fs.unlinkSync(destination);
        console.log(`Removed stale CUDA runtime: ${name}`);
      }
    }
    return;
  }

  const cudaPath = process.env.CUDA_PATH;
  if (!cudaPath) {
    throw new Error('CUDA_PATH is required when LUMINA_DESKTOP_CUDA=1');
  }

  const cudaBinCandidates = [path.join(cudaPath, 'bin', 'x64'), path.join(cudaPath, 'bin')];
  const cudaBin = cudaBinCandidates.find((candidate) =>
    windowsCudaRuntimeFiles.every((name) => fs.existsSync(path.join(candidate, name)))
  );
  if (!cudaBin) {
    throw new Error(
      `CUDA runtime is incomplete under ${cudaPath}; required: ${windowsCudaRuntimeFiles.join(', ')}`
    );
  }

  for (const name of windowsCudaRuntimeFiles) {
    fs.copyFileSync(path.join(cudaBin, name), path.join(srcBinDir, name));
    console.log(`Copied CUDA runtime: ${name}`);
  }
}

// Helper function to clean directory of cross-platform files
function cleanBinDirectory(targetPlatform) {
  console.log(`Cleaning bin directory for ${targetPlatform} build...`);

  if (!fs.existsSync(srcBinDir)) {
    console.log('src/bin directory does not exist, skipping cleanup');
    return;
  }

  const files = fs.readdirSync(srcBinDir, { withFileTypes: true });

  files.forEach((file) => {
    const filePath = path.join(srcBinDir, file.name);
    const normalizedName = file.name.toLowerCase();

    if (targetPlatform === 'darwin' || targetPlatform === 'linux') {
      if (matchesPattern(file.name, windowsFiles)) {
        console.log(`Removing Windows file: ${file.name}`);
        if (file.isDirectory()) {
          fs.rmSync(filePath, { recursive: true, force: true });
        } else {
          fs.unlinkSync(filePath);
        }
      }
    } else if (targetPlatform === 'win32') {
      if (
        file.isFile() &&
        path.extname(normalizedName) === '.exe' &&
        !allowedWindowsExecutables.has(normalizedName)
      ) {
        console.log(`Removing unapproved Windows executable: ${file.name}`);
        fs.unlinkSync(filePath);
        return;
      }

      // For Windows, remove macOS-specific files (keep only Windows files and common files)
      if (
        !matchesPattern(file.name, windowsFiles) &&
        !matchesPattern(file.name, ['*.db', '*.log', '.gitkeep'])
      ) {
        // Check if it's a macOS binary (executable without extension)
        if (file.isFile() && !path.extname(file.name) && file.name !== '.gitkeep') {
          try {
            // Check if file is executable (likely a macOS binary)
            const stats = fs.statSync(filePath);
            if (stats.mode & parseInt('111', 8)) {
              // Check if any execute bit is set
              console.log(`Removing macOS binary: ${file.name}`);
              fs.unlinkSync(filePath);
            }
          } catch (err) {
            console.warn(`Could not check file ${file.name}:`, err.message);
          }
        }
      }
    }
  });
}

// Helper function to copy platform-specific files
async function copyPlatformFiles(targetPlatform) {
  if (targetPlatform === 'win32') {
    console.log('Copying Windows-specific files...');

    if (!fs.existsSync(platformWinDir)) {
      console.warn('Windows platform directory does not exist');
      return;
    }

    // Ensure src/bin exists
    if (!fs.existsSync(srcBinDir)) {
      fs.mkdirSync(srcBinDir, { recursive: true });
    }

    // Copy Windows-specific scripts and authored support files.
    const files = fs.readdirSync(platformWinDir, { withFileTypes: true });
    files.forEach((file) => {
      if (
        file.name === 'README.md' ||
        file.name === '.gitignore' ||
        file.name.endsWith('.exe') ||
        file.name.endsWith('.dll')
      ) {
        return;
      }

      const srcPath = path.join(platformWinDir, file.name);
      const destPath = path.join(srcBinDir, file.name);

      if (file.isDirectory()) {
        fs.cpSync(srcPath, destPath, { recursive: true, force: true });
        console.log(`Copied directory: ${file.name}`);
      } else {
        fs.copyFileSync(srcPath, destPath);
        console.log(`Copied: ${file.name}`);
      }
    });

    await ensureWindowsUvBinaries();
    syncWindowsCudaRuntimeBinaries();
    writeOrValidateBackendBuildStamp();
  }
}

// Main function
async function preparePlatformBinaries() {
  const targetPlatform = process.env.ELECTRON_PLATFORM || process.platform;

  console.log(`Preparing binaries for platform: ${targetPlatform}`);

  // First copy platform-specific files if needed
  await copyPlatformFiles(targetPlatform);

  // Then clean up cross-platform files
  cleanBinDirectory(targetPlatform);

  console.log('Platform binary preparation complete');
}

// Run if called directly
if (require.main === module) {
  preparePlatformBinaries().catch((error) => {
    console.error(error);
    process.exit(1);
  });
}

module.exports = { preparePlatformBinaries };
