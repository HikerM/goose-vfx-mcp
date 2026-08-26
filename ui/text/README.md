# lumina ACP TUI

Early stage and part of lumina's broader move to ACP

https://github.com/HikerM/lumina/issues/6642
https://github.com/HikerM/lumina/discussions/7309

## Running

The TUI launches the lumina ACP server by spawning `lumina acp`. Which binary it spawns is resolved by `@hikerm/lumina-sdk`:

1. the `LUMINA_BINARY` environment variable, if set, otherwise
2. the platform's prebuilt `@hikerm/lumina-binary-*` package (an optional dependency of the pinned `@hikerm/lumina-sdk`).

```bash
cd ui/text
pnpm install   # links the in-repository Lumina SDK and matching binary packages
pnpm start     # tsx src/tui.tsx — runs against the released binary, no Rust build
```

The TUI uses the workspace `@hikerm/lumina-sdk`, so local builds always run against a Lumina binary that matches the SDK source.

### Building lumina from local source

To test local Rust changes, run the dev launcher directly. It builds a debug binary (`cargo build -p lumina-cli` → `target/debug/lumina`) from the workspace root and points the TUI at it via `LUMINA_BINARY`:

```bash
node scripts/dev-start.mjs
```

If your changes touch the ACP schema, regenerate the SDK and update the Rust schema, generated TypeScript, Desktop adapters, and TUI in the same change.

To run any other prebuilt binary, set `LUMINA_BINARY=/path/to/lumina` and use `pnpm start`.

### Custom server URL

To connect to an already-running server instead of spawning a binary:

```bash
pnpm start -- --server http://localhost:8080
```
