# lumina Desktop App

Native desktop app for lumina built with [Electron](https://www.electronjs.org/) and [ReactJS](https://react.dev/).

# Building and running
lumina uses [Hermit](https://github.com/cashapp/hermit) to manage dependencies, so you will need to have it installed and activated.

### Linux/macOS quick start

```
git clone git@github.com:HikerM/lumina.git
cd lumina
source ./bin/activate-hermit
cd ui/desktop
pnpm install
pnpm run start
```

## Platform-specific build requirements

### Linux
For building on Linux distributions, you'll need additional system dependencies:

**Debian/Ubuntu:**
```bash
sudo apt install dpkg fakeroot
```

**Arch/Manjaro:**
```bash
sudo pacman -S dpkg fakeroot
```

**Fedora/RHEL:**
```bash
sudo dnf install dpkg-dev fakeroot
```

# Building notes

This is an Electron Forge app using Vite and React. The desktop app launches the bundled `lumina` CLI binary and talks to its ACP server.

## Building for different platforms

### macOS
`pnpm run bundle:default` will give you a lumina.app/zip which is signed/notarized but only if you set up the env vars as per `forge.config.ts` (you can empty out the section on osxSign if you don't want to sign it) - this will have all defaults.

`pnpm run bundle:preconfigured` will make a lumina.app/zip signed and notarized, but use the following:

```python
            f"        process.env.LUMINA_PROVIDER__TYPE = '{os.getenv("LUMINA_BUNDLE_TYPE")}';",
            f"        process.env.LUMINA_PROVIDER__HOST = '{os.getenv("LUMINA_BUNDLE_HOST")}';",
            f"        process.env.LUMINA_PROVIDER__MODEL = '{os.getenv("LUMINA_BUNDLE_MODEL")}';"
```

This allows you to set for example LUMINA_PROVIDER__TYPE to be "databricks" by default if you want (so when people start lumina.app - they will get that out of the box). There is no way to set an api key in that bundling as that would be a terrible idea, so only use providers that can do oauth (like databricks can), otherwise stick to default lumina.

### Linux
For Linux builds, first ensure you have the required system dependencies installed (see above), then:

1. Build the Rust binary:
```bash
cd ../..  # Go to project root
cargo build --release -p lumina-cli --bin lumina
```

2. Copy the binary to the expected location:
```bash
mkdir -p src/bin
cp ../../target/release/lumina src/bin/
```

3. Build the application:
```bash
# For ZIP distribution (works on all Linux distributions)
pnpm run make --targets=@electron-forge/maker-zip

# For DEB package (Debian/Ubuntu)
pnpm run make --targets=@electron-forge/maker-deb

# For Flatpak (requires flatpak and flatpak-builder)
pnpm run make --targets=@electron-forge/maker-flatpak
```

The built application will be available in:
- ZIP: `out/make/zip/linux/x64/lumina-linux-x64-{version}.zip`
- DEB: `out/make/deb/x64/lumina_{version}_amd64.deb`
- Flatpak: `out/make/flatpak/x86_64/*.flatpak`
- Executable: `out/lumina-linux-x64/lumina`

### Windows
From PowerShell, use the repository Windows wrapper. It resolves Rust from
`D:\DevTools\Rust` and reports the exact paths it checked if the toolchain is
missing:

```powershell
. .\bin\activate-lumina-rust.ps1
& .\bin\cargo.cmd build --release -p lumina-cli --bin lumina
cd ui\desktop
pnpm install
pnpm run start-gui
```

Do not run `bin\cargo` without an extension on Windows; it is the POSIX
placeholder and can trigger the Windows file-association dialog. If the D:
drive toolchain is missing, install Rust with `rustup-init.exe` configured for
`D:\DevTools\Rust` before retrying. These wrappers do not use a C: drive Rust
installation or modify PATH permanently.


# Running with an external ACP backend

From the project root, start the ACP backend:

### Linux/macOS

```bash
LUMINA_SERVER__SECRET_KEY=test cargo run -p lumina-cli --bin lumina -- serve --platform desktop --host 127.0.0.1 --port 3000
```

### Windows PowerShell

```powershell
. .\bin\activate-lumina-rust.ps1
if ([string]::IsNullOrEmpty($env:LUMINA_SERVER__SECRET_KEY)) { $env:LUMINA_SERVER__SECRET_KEY = "test" }
& .\bin\cargo.cmd run -p lumina-cli --bin lumina -- serve --platform desktop --host 127.0.0.1 --port 3000
```

### Linux/macOS desktop app

Then start the desktop app from `ui/desktop`:

```bash
LUMINA_EXTERNAL_BACKEND=true LUMINA_EXTERNAL_BACKEND_URL=http://127.0.0.1:3000 LUMINA_SERVER__SECRET_KEY=test pnpm run start-gui
```

On Windows PowerShell, run the same app command after changing to
`ui\desktop`:

```powershell
$env:LUMINA_EXTERNAL_BACKEND = "true"
$env:LUMINA_EXTERNAL_BACKEND_URL = "http://127.0.0.1:3000"
$env:LUMINA_SERVER__SECRET_KEY = "test"
pnpm run start-gui
```
