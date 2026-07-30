$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "goose-rust-tools.ps1")

$tools = Initialize-GooseRustEnvironment

Write-Host "Goose Rust environment activated." -ForegroundColor Green
Write-Host "  cargo.exe : $($tools.CargoExe)"
Write-Host "  rustup.exe: $($tools.RustupExe)"
Write-Host "  CARGO_HOME: $env:CARGO_HOME"
Write-Host "  RUSTUP_TOOLCHAIN: $env:RUSTUP_TOOLCHAIN"
if (-not [string]::IsNullOrWhiteSpace($env:RUSTUP_HOME)) {
    Write-Host "  RUSTUP_HOME: $env:RUSTUP_HOME"
}
Write-Host "  PATH starts with: $($tools.CargoBin)"
Write-Host "  Repo bin placeholders are excluded from PATH for this shell."
