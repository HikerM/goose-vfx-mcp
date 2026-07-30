# Windows-Specific Runtime Files

This directory contains Windows-specific scripts that are only included during Windows builds.

## Components

### Node.js Runtime
- `npx.cmd` - Fail-closed wrapper for the Goose-managed portable Node.js runtime

`npx.cmd` resolves `GOOSE_PATH_ROOT` through the same Windows storage policy as
the desktop app. When unset it uses `D:\Goose`; configured roots must be local
folders on the D drive. It uses only:

```text
<root>\runtime\node
<root>\runtime\npm-cache
<root>\tmp
```

The portable runtime must contain `node.exe`, `npm.cmd`, `npx.cmd`, npm's full
dependency tree, and a strict `manifest.json` for the pinned archive. On every
invocation the forwarder checks the manifest, pinned archive SHA, every file's
SHA-256 value, and rejects reparse points from `D:\` through the complete
runtime path. If
anything is absent, changed, or unverifiable, it fails with a user-visible
request to run `prepare-windows-npm.ps1` again. It never uses
system Node.js, a user-profile directory, the default Windows temp directory, or an
unverified download. Runtime provisioning must therefore be performed by the
packaging mechanism with its existing signed/fixed-integrity process.

The cache is `<root>\runtime\npm-cache`; temporary files are `<root>\tmp`; the
shim itself and `npx-forward.ps1` are copied consistently as a staged set to
`<root>\bin`. No cache/temp directory or controlled environment variable is
created/set until the full runtime integrity check succeeds. `PATH`, npm cache,
and temp variables are injected only into the stdio MCP child environment;
harmless existing stdio environment entries are retained.
The prepare script validates staging before publication, keeps the last complete
runtime in `.node-previous`, and restores it after an interrupted publication.
Windows directory moves are not an atomic replacement of a non-empty directory;
the script therefore treats publication as a recoverable transaction. The
PowerShell path audit cannot hold a no-follow executable handle across
`CreateProcess`; protect `D:\Goose` with normal user ACLs and rerun prepare if
an integrity check fails.

### Windows Binaries
- `uv.exe` and `uvx.exe` are downloaded from the pinned Astral uv release during packaging.
- Compiled `.exe` and `.dll` files are generated or fetched during the build and are not committed.

## Build Process

Windows runtime files are prepared during the build process by:
1. `prepare-windows-npm.sh` - Copies the authored Windows runtime shim
2. `prepare-platform-binaries.js` - Downloads pinned uv binaries and copies Windows-specific files to `src/bin`

The authored shim sources are committed; packaging copies them into the
installed `<root>\bin` directory. The portable Node runtime and uv binaries are
still generated/fetched during packaging and are not committed.
