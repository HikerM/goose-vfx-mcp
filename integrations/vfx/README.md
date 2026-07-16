# VFX MCP integrations

Run `scripts/vfx/install-vfx-mcps.ps1` from the repository root. It clones the commits locked in `manifest.json` into `.vfx-runtime`, creates isolated environments, and installs selected DCC plugins without overwriting existing files.

The Unreal extension is listed in Goose as disabled and uses only `http://127.0.0.1:3000/mcp`. The installer writes the Houdini and Nuke stdio extensions into the user's Goose configuration with absolute paths generated on that machine; no machine-specific paths are committed.

In Unreal, enable MCP Automation Bridge, Python Editor Script Plugin, Editor Scripting Utilities, and Niagara; then enable Native MCP in Project Settings on port 3000 and restart. In Houdini, the package starts the loopback server on port 8100. In Nuke, use the Script Editor: `import nuke_mcp_addon; nuke_mcp_addon.start()`.
