$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "goose-rust-tools.ps1")

$tools = Initialize-GooseRustEnvironment
& $tools.RustupExe @args
exit $LASTEXITCODE
