Set-StrictMode -Version Latest

function Get-GooseRustCandidateHomes {
    return [System.Collections.Generic.List[string]]@("D:\DevTools\Rust\cargo")
}

function Get-GooseRustupCandidateHomes {
    return [System.Collections.Generic.List[string]]@("D:\DevTools\Rust\rustup")
}

function Resolve-GooseRustBinary {
    param(
        [Parameter(Mandatory = $true)]
        [string]$BinaryName,
        [Parameter(Mandatory = $true)]
        [System.Collections.Generic.List[string]]$CandidateHomes
    )

    $checked = New-Object System.Collections.Generic.List[string]

    foreach ($candidateHome in $CandidateHomes) {
        $candidate = Join-Path $candidateHome "bin\$BinaryName"
        $checked.Add($candidate)

        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return [PSCustomObject]@{
                Path = (Resolve-Path -LiteralPath $candidate).Path
                Home = $candidateHome
                Checked = $checked
            }
        }
    }

    throw "Could not find $BinaryName in the D: drive toolchain. Checked:`n - $($checked -join "`n - ")`nInstall the official Rust toolchain under D:\DevTools\Rust\cargo and retry. This entry point does not use a C: drive Rust installation."
}

function Resolve-GooseRustHome {
    param(
        [Parameter(Mandatory = $true)]
        [string]$HomeName,
        [Parameter(Mandatory = $true)]
        [System.Collections.Generic.List[string]]$CandidateHomes
    )

    $checked = New-Object System.Collections.Generic.List[string]

    foreach ($candidateHome in $CandidateHomes) {
        $checked.Add($candidateHome)

        if (Test-Path -LiteralPath $candidateHome -PathType Container) {
            return [PSCustomObject]@{
                Path = (Resolve-Path -LiteralPath $candidateHome).Path
                Checked = $checked
            }
        }
    }

    throw "Could not find $HomeName in the D: drive toolchain. Checked:`n - $($checked -join "`n - ")`nInstall the official Rust toolchain under D:\DevTools\Rust and retry. This entry point does not use a C: drive Rust installation."
}

function Normalize-GoosePath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$PathValue
    )

    return [System.IO.Path]::GetFullPath($PathValue).TrimEnd("\")
}

function Test-GooseRustPath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$PathValue
    )

    $normalized = Normalize-GoosePath -PathValue $PathValue
    if (-not $normalized.StartsWith("C:\", [System.StringComparison]::OrdinalIgnoreCase)) {
        return $false
    }

    return $normalized -match '(?i)(?:^|[\\/])(?:\.cargo|cargo|rustup|rust)(?:[\\/]|$)'
}

function Assert-GooseRustBinary {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Name,
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $normalized = Normalize-GoosePath -PathValue $Path
    if (-not $normalized.StartsWith("D:\DevTools\", [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "$Name resolved outside D:\DevTools: $normalized"
    }

    $previousErrorActionPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & $Path --version *> $null
    $exitCode = $LASTEXITCODE
    $ErrorActionPreference = $previousErrorActionPreference
    if ($exitCode -ne 0) {
        throw "$Name is not usable: $normalized"
    }
}

function Get-GooseRustTools {
    $candidateHomes = Get-GooseRustCandidateHomes
    $rustupHomes = Get-GooseRustupCandidateHomes
    $cargo = Resolve-GooseRustBinary -BinaryName "cargo.exe" -CandidateHomes $candidateHomes
    $rustup = Resolve-GooseRustBinary -BinaryName "rustup.exe" -CandidateHomes $candidateHomes
    $rustc = Resolve-GooseRustBinary -BinaryName "rustc.exe" -CandidateHomes $candidateHomes
    $rustfmt = Resolve-GooseRustBinary -BinaryName "rustfmt.exe" -CandidateHomes $candidateHomes
    $clippyDriver = Resolve-GooseRustBinary -BinaryName "clippy-driver.exe" -CandidateHomes $candidateHomes
    $rustupHome = Resolve-GooseRustHome -HomeName "RUSTUP_HOME" -CandidateHomes $rustupHomes

    Assert-GooseRustBinary -Name "cargo" -Path $cargo.Path
    Assert-GooseRustBinary -Name "rustup" -Path $rustup.Path
    Assert-GooseRustBinary -Name "rustc" -Path $rustc.Path
    Assert-GooseRustBinary -Name "rustfmt" -Path $rustfmt.Path
    Assert-GooseRustBinary -Name "clippy-driver" -Path $clippyDriver.Path

    return [PSCustomObject]@{
        CargoExe = $cargo.Path
        RustupExe = $rustup.Path
        RustcExe = $rustc.Path
        RustfmtExe = $rustfmt.Path
        ClippyDriverExe = $clippyDriver.Path
        CargoHome = $cargo.Home
        CargoBin = Split-Path -Parent $cargo.Path
        RustupHome = $rustupHome.Path
        RepoBin = (Resolve-Path -LiteralPath $PSScriptRoot).Path
    }
}

function Initialize-GooseRustEnvironment {
    $tools = Get-GooseRustTools
    $repoBin = Normalize-GoosePath -PathValue $tools.RepoBin
    $cargoBin = Normalize-GoosePath -PathValue $tools.CargoBin

    $env:CARGO_HOME = $tools.CargoHome
    $env:RUSTUP_HOME = $tools.RustupHome
    $env:RUSTUP_TOOLCHAIN = "1.94.1"

    $pathEntries = New-Object System.Collections.Generic.List[string]
    $pathEntries.Add($tools.CargoBin)

    foreach ($entry in ($env:PATH -split ";")) {
        if ([string]::IsNullOrWhiteSpace($entry)) {
            continue
        }

        $normalizedEntry = Normalize-GoosePath -PathValue $entry
        if ($normalizedEntry -ieq $cargoBin -or $normalizedEntry -ieq $repoBin -or (Test-GooseRustPath -PathValue $entry)) {
            continue
        }

        $pathEntries.Add($entry)
    }

    $env:PATH = $pathEntries -join ";"
    return $tools
}
