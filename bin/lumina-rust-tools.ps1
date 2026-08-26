Set-StrictMode -Version Latest

function Get-LuminaRustCandidateHomes {
    return [System.Collections.Generic.List[string]]@("D:\DevTools\Rust\cargo")
}

function Get-LuminaRustupCandidateHomes {
    return [System.Collections.Generic.List[string]]@("D:\DevTools\Rust\rustup")
}

function Resolve-LuminaRustBinary {
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

function Resolve-LuminaRustHome {
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

function Normalize-LuminaPath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$PathValue
    )

    return [System.IO.Path]::GetFullPath($PathValue).TrimEnd("\")
}

function Test-LuminaRustPath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$PathValue
    )

    $normalized = Normalize-LuminaPath -PathValue $PathValue
    if (-not $normalized.StartsWith("C:\", [System.StringComparison]::OrdinalIgnoreCase)) {
        return $false
    }

    return $normalized -match '(?i)(?:^|[\\/])(?:\.cargo|cargo|rustup|rust)(?:[\\/]|$)'
}

function Assert-LuminaRustBinary {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Name,
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $normalized = Normalize-LuminaPath -PathValue $Path
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

function Get-LuminaRustTools {
    $candidateHomes = Get-LuminaRustCandidateHomes
    $rustupHomes = Get-LuminaRustupCandidateHomes
    $cargo = Resolve-LuminaRustBinary -BinaryName "cargo.exe" -CandidateHomes $candidateHomes
    $rustup = Resolve-LuminaRustBinary -BinaryName "rustup.exe" -CandidateHomes $candidateHomes
    $rustc = Resolve-LuminaRustBinary -BinaryName "rustc.exe" -CandidateHomes $candidateHomes
    $rustfmt = Resolve-LuminaRustBinary -BinaryName "rustfmt.exe" -CandidateHomes $candidateHomes
    $clippyDriver = Resolve-LuminaRustBinary -BinaryName "clippy-driver.exe" -CandidateHomes $candidateHomes
    $rustupHome = Resolve-LuminaRustHome -HomeName "RUSTUP_HOME" -CandidateHomes $rustupHomes

    Assert-LuminaRustBinary -Name "cargo" -Path $cargo.Path
    Assert-LuminaRustBinary -Name "rustup" -Path $rustup.Path
    Assert-LuminaRustBinary -Name "rustc" -Path $rustc.Path
    Assert-LuminaRustBinary -Name "rustfmt" -Path $rustfmt.Path
    Assert-LuminaRustBinary -Name "clippy-driver" -Path $clippyDriver.Path

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

function Initialize-LuminaRustEnvironment {
    $tools = Get-LuminaRustTools
    $repoBin = Normalize-LuminaPath -PathValue $tools.RepoBin
    $cargoBin = Normalize-LuminaPath -PathValue $tools.CargoBin

    $env:CARGO_HOME = $tools.CargoHome
    $env:RUSTUP_HOME = $tools.RustupHome
    $env:RUSTUP_TOOLCHAIN = "1.94.1"
    if ([string]::IsNullOrWhiteSpace($env:CXXFLAGS)) {
        $env:CXXFLAGS = "/utf-8 /EHsc"
    }

    $pathEntries = New-Object System.Collections.Generic.List[string]
    $pathEntries.Add($tools.CargoBin)

    foreach ($entry in ($env:PATH -split ";")) {
        if ([string]::IsNullOrWhiteSpace($entry)) {
            continue
        }

        $normalizedEntry = Normalize-LuminaPath -PathValue $entry
        if ($normalizedEntry -ieq $cargoBin -or $normalizedEntry -ieq $repoBin -or (Test-LuminaRustPath -PathValue $entry)) {
            continue
        }

        $pathEntries.Add($entry)
    }

    $env:PATH = $pathEntries -join ";"
    return $tools
}
