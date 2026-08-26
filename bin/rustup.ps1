$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "lumina-rust-tools.ps1")

$tools = Initialize-LuminaRustEnvironment
& $tools.RustupExe @args
exit $LASTEXITCODE
