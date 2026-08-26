$ErrorActionPreference = "Stop"

$validateCmdShim = $args.Count -eq 1 -and $args[0] -eq "-ValidateCmdShim"

if ($validateCmdShim) {
    $current = Get-CimInstance Win32_Process -Filter "ProcessId=$PID"
    $parent = if ($null -ne $current -and $current.ParentProcessId) {
        Get-CimInstance Win32_Process -Filter "ProcessId=$($current.ParentProcessId)"
    }
    $raw = if ($null -ne $parent) { $parent.CommandLine } else { $null }
    if ([string]::IsNullOrEmpty($raw)) {
        [Console]::Error.WriteLine("cargo.cmd could not inspect its parent command line; use powershell.exe -File bin\cargo.ps1 for programmatic argv.")
        exit 2
    }
    if ($raw -match '[\x00-\x1f&|<>^()%!$`]') {
        [Console]::Error.WriteLine("cargo.cmd rejected shell metacharacters or control characters; use powershell.exe -File bin\cargo.ps1 for programmatic argv.")
        exit 2
    }
    exit 0
}

. (Join-Path $PSScriptRoot "lumina-rust-tools.ps1")

$tools = Initialize-LuminaRustEnvironment
& $tools.CargoExe @args
exit $LASTEXITCODE
