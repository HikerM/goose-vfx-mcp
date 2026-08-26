# AGENTS Instructions

lumina is an AI agent framework in Rust with CLI and Electron desktop interfaces.

## Windows Rust Entry
For this Windows Codex workspace, use only these Rust entry points:

```powershell
. .\bin\activate-lumina-rust.ps1
& .\bin\cargo.cmd +1.94.1 --version
& 'D:\DevTools\Rust\cargo\bin\cargo.exe' -V
```

- On Windows PowerShell, use exactly one of these flows:
- Dot-source `. .\bin\activate-lumina-rust.ps1` in the current shell, then run `cargo ...`.
- Or run `& .\bin\cargo.cmd <args>` each time without changing the current shell.
- Never run `source bin/activate-hermit` on Windows.
- Never run `bin\cargo`, `.\bin\cargo`, or any extensionless `cargo` placeholder from this repository on Windows. `D:\Project\lumina\bin\cargo` is a Unix-only placeholder, not an executable.
- All Rust, rustup, and LLVM tools used on Windows must resolve from `D:\DevTools\...`, not from `D:\Project\lumina\bin`.
- If Codex or PowerShell was started before the PATH was fixed, fully exit and reopen Codex before relying on bare `cargo` after activation.
- Ignore the macOS/Linux-only POSIX setup below unless the user explicitly asks for Linux or macOS instructions.

## Setup
### Windows PowerShell
```powershell
. .\bin\activate-lumina-rust.ps1
cargo build
# or, without activating the shell:
& .\bin\cargo.cmd build
```

### macOS/Linux only (POSIX)
```bash
source bin/activate-hermit
cargo build
```

## Commands

### Build
```powershell
cargo build
cargo build --release
# if the shell is not activated:
& .\bin\cargo.cmd build
& .\bin\cargo.cmd build --release
just release-binary           # release binary
```

### Test
```powershell
cargo test
cargo test -p lumina
cargo test --package lumina --test mcp_integration_test
# if the shell is not activated:
& .\bin\cargo.cmd test
& .\bin\cargo.cmd test -p lumina
& .\bin\cargo.cmd test --package lumina --test mcp_integration_test
just record-mcp-tests        # record MCP
```

### Lint/Format
```powershell
cargo fmt
cargo clippy --all-targets -- -D warnings
# if the shell is not activated:
& .\bin\cargo.cmd fmt
& .\bin\cargo.cmd clippy --all-targets -- -D warnings
```

### UI
```bash
just run-ui                  # start desktop
cd ui/desktop && pnpm run typecheck
cd ui/desktop && pnpm test   # test UI
```

## Structure
```
crates/
├── lumina              # core logic
├── lumina-acp-macros   # ACP proc macros
├── lumina-cli          # CLI entry
├── lumina-mcp          # MCP extensions
├── lumina-test         # test utilities
└── lumina-test-support # test helpers

ui/desktop/            # Electron app
```

## Development Loop
```powershell
# 1. . .\bin\activate-lumina-rust.ps1
# 2. Make changes
# 3. cargo fmt
```

### Run these only if the user has asked you to build/test your changes:
```
# 1. cargo build
# 2. cargo test -p <crate>
# 3. cargo clippy --all-targets -- -D warnings
```

## Rules

- Test: Prefer tests/ folder, e.g. crates/lumina/tests/
- Test: When adding features, update lumina-self-test.yaml, rebuild, then run `lumina run --recipe lumina-self-test.yaml` to validate
- Error: Use anyhow::Result
- Provider: Implement Provider trait see providers/base.rs
- MCP: Extensions in crates/lumina-mcp/
- UI Desktop: Use ACP SDK types or local `src/types/*` types. Do not import generated OpenAPI types/client code from `ui/desktop/src/api`

## Code Quality

- Comments: Write self-documenting code - prefer clear names over comments
- Comments: Never add comments that restate what code does
- Comments: Only comment for complex algorithms, non-obvious business logic, or "why" not "what"
- Simplicity: Don't make things optional that don't need to be - the compiler will enforce
- Simplicity: Booleans should default to false, not be optional
- Errors: Don't add error context that doesn't add useful information (e.g., `.context("Failed to X")` when error already says it failed)
- Simplicity: Avoid overly defensive code - trust Rust's type system
- Logging: Clean up existing logs, don't add more unless for errors or security events

## Ink / Terminal UI (ui/text)

- Ink renders React to a fixed character grid — not a browser. Content that exceeds a Box's dimensions is NOT clipped; it visually overflows into neighboring cells and breaks the layout.

- Ink-Text: Never use `wrap="wrap"` inside a fixed-height Box — wrapped text can exceed the Box height and bleed into adjacent components. Use `wrap="truncate"` and pre-truncate the string to fit the available character budget (lines × width).
  
- Ink-Layout: When changing card/cell dimensions, always recalculate how much content fits. Account for borders (2 chars), padding, margins, and sibling elements when computing the
remaining space for dynamic text.
  
- Ink-Overflow: Ink has no `overflow: hidden`. The only way to prevent overflow is to ensure content never exceeds the container size — truncate text, limit list items, or cap height.
  
- Ink-FlexGrow: Avoid `flexGrow={1}` on text containers inside fixed-height cards — the text will try to fill available space but Ink won't clip it if it exceeds the boundary.
  
- Ink-HeightBudget: When computing how many rows/items fit vertically, count EVERY line used by headers, footers, margins, borders, and scroll indicators. Under-reserving vertical space (e.g., `height - 8` when chrome actually uses 16 lines) causes Ink to squeeze out margins between items, making borders collapse. Always audit the actual line count.
  
- Ink-TrailingMargin: Don't apply `marginBottom` to the last item in a list — it wastes a line and can push content out of the container. Use conditional margins or container `gap`.

## Never

- Never: Recreate `ui/desktop/src/api` or add `@hey-api/openapi-ts` to `ui/desktop`
- Cargo.toml: For human-authored dependency changes on Windows, use the `D:\DevTools\Rust\cargo\bin\cargo.exe add ...` entry pattern instead of manually editing dependency entries unless there is a specific reason not to.
- Cargo.toml: Automated dependency bump PRs are exempt; when manual edits are necessary, keep `Cargo.lock` consistent.
- Never: Skip the required Windows formatting step from the Commands section.
- Never: Merge without running the required Windows clippy step from the Commands section.
- Never: Comment self-evident operations (`// Initialize`, `// Return result`), getters/setters, constructors, or standard Rust idioms

## Entry Points
- CLI: crates/lumina-cli/src/main.rs
- UI: ui/desktop/src/main.ts
- Agent: crates/lumina/src/agents/agent.rs
