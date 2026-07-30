#!/bin/bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if command -v powershell.exe >/dev/null 2>&1; then
  powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$(wslpath -w "$SCRIPT_DIR/prepare-windows-npm.ps1")"
else
  echo 'prepare-windows-npm.sh must run under Windows/WSL with powershell.exe available' >&2
  exit 1
fi
