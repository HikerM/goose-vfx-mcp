[CmdletBinding()]
param(
    [Parameter()]
    [ValidateSet('Auto', 'Cuda', 'Cpu')]
    [string]$Acceleration = 'Auto',

    [Parameter()]
    [string]$RepositoryRoot = '',

    [Parameter()]
    [switch]$ValidateOnly
)

$ErrorActionPreference = 'Stop'

function Write-BuildMessage {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Message
    )

    [Console]::Error.WriteLine($Message)
}

function Invoke-CheckedCommand {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$FilePath,

        [Parameter()]
        [string[]]$ArgumentList = @()
    )

    & $FilePath @ArgumentList
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code ${LASTEXITCODE}: $FilePath"
    }
}

function Get-RequiredCommandPath {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Name
    )

    $command = Get-Command -Name $Name -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($null -eq $command) {
        throw "Required command is unavailable: $Name"
    }

    return $command.Source
}

function Initialize-VisualCppEnvironment {
    [CmdletBinding()]
    param()

    $existingCompiler = Get-Command -Name 'cl.exe' -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($null -ne $existingCompiler) {
        return
    }

    $vswhereCandidates = @(
        (Join-Path -Path ${env:ProgramFiles(x86)} -ChildPath 'Microsoft Visual Studio\Installer\vswhere.exe'),
        (Join-Path -Path $env:ProgramFiles -ChildPath 'Microsoft Visual Studio\Installer\vswhere.exe')
    )
    $vswherePath = $vswhereCandidates |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
        Select-Object -First 1
    if ([string]::IsNullOrWhiteSpace($vswherePath)) {
        throw 'CUDA packaging requires Visual Studio Build Tools with the Desktop development with C++ workload.'
    }

    $visualStudioPath = & $vswherePath `
        -latest `
        -products '*' `
        -requires 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64' `
        -property installationPath
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($visualStudioPath)) {
        throw 'Visual Studio with the x64 C++ toolchain could not be located.'
    }

    $devShellModule = Join-Path -Path $visualStudioPath.Trim() -ChildPath 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll'
    if (-not (Test-Path -LiteralPath $devShellModule -PathType Leaf)) {
        throw "Visual Studio developer shell module is unavailable: $devShellModule"
    }

    Import-Module -Name $devShellModule -Force
    Enter-VsDevShell `
        -VsInstallPath $visualStudioPath.Trim() `
        -SkipAutomaticLocation `
        -DevCmdArguments '-arch=x64 -host_arch=x64'

    $compiler = Get-Command -Name 'cl.exe' -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($null -eq $compiler) {
        throw 'Visual Studio developer environment loaded without cl.exe.'
    }
}

function Get-CudaConfiguration {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [ValidateSet('Auto', 'Cuda', 'Cpu')]
        [string]$RequestedAcceleration
    )

    $runtimeNames = @(
        'cublas64_12.dll',
        'cublasLt64_12.dll',
        'cudart64_12.dll',
        'curand64_10.dll'
    )
    $cudaRoot = $env:CUDA_PATH
    $nvccPath = $null
    $runtimePaths = @()

    if (-not [string]::IsNullOrWhiteSpace($cudaRoot)) {
        $candidateNvcc = Join-Path -Path $cudaRoot -ChildPath 'bin\nvcc.exe'
        if (Test-Path -LiteralPath $candidateNvcc -PathType Leaf) {
            $nvccPath = $candidateNvcc
        }

        foreach ($runtimeName in $runtimeNames) {
            $relativeRuntimePath = Join-Path -Path 'bin' -ChildPath $runtimeName
            $runtimePath = Join-Path -Path $cudaRoot -ChildPath $relativeRuntimePath
            if (Test-Path -LiteralPath $runtimePath -PathType Leaf) {
                $runtimePaths += $runtimePath
            }
        }
    }

    $cudaReady = ($null -ne $nvccPath) -and ($runtimePaths.Count -eq $runtimeNames.Count)
    if (($RequestedAcceleration -eq 'Cuda') -and (-not $cudaReady)) {
        throw 'CUDA was requested, but CUDA_PATH, nvcc.exe, or the required CUDA 12 runtime DLLs are unavailable.'
    }

    $useCuda = $cudaReady -and ($RequestedAcceleration -ne 'Cpu')
    $resolvedAcceleration = 'Cpu'
    if ($useCuda) {
        $resolvedAcceleration = 'Cuda'
    }

    return [pscustomobject]@{
        RequestedAcceleration = $RequestedAcceleration
        ResolvedAcceleration = $resolvedAcceleration
        CudaReady = $cudaReady
        CudaRoot = $cudaRoot
        NvccPath = $nvccPath
        RuntimePaths = $runtimePaths
    }
}

if ([string]::IsNullOrWhiteSpace($RepositoryRoot)) {
    $RepositoryRoot = Split-Path -Parent $PSScriptRoot
}

$resolvedRepositoryRoot = (Resolve-Path -LiteralPath $RepositoryRoot).Path
$desktopRoot = Join-Path -Path $resolvedRepositoryRoot -ChildPath 'ui\desktop'
$rustToolsScript = Join-Path -Path $resolvedRepositoryRoot -ChildPath 'bin\lumina-rust-tools.ps1'
$releaseTargetDirectory = Join-Path -Path $resolvedRepositoryRoot -ChildPath 'target\lumina-windows-release'
$backendBinary = Join-Path -Path $releaseTargetDirectory -ChildPath 'release\lumina.exe'
$desktopBinaryDirectory = Join-Path -Path $desktopRoot -ChildPath 'src\bin'
$desktopBinary = Join-Path -Path $desktopBinaryDirectory -ChildPath 'lumina.exe'
$packagedApp = Join-Path -Path $desktopRoot -ChildPath 'out\Lumina-win32-x64\Lumina.exe'
$installerDirectory = Join-Path -Path $desktopRoot -ChildPath 'out\make\inno'
$cudaConfiguration = Get-CudaConfiguration -RequestedAcceleration $Acceleration

if ($ValidateOnly) {
    [pscustomobject]@{
        repositoryRoot = $resolvedRepositoryRoot
        requestedAcceleration = $cudaConfiguration.RequestedAcceleration
        resolvedAcceleration = $cudaConfiguration.ResolvedAcceleration
        cudaReady = $cudaConfiguration.CudaReady
        cpuFallback = $true
    } | ConvertTo-Json -Compress
    return
}

if (-not (Test-Path -LiteralPath $desktopRoot -PathType Container)) {
    throw "Desktop project is unavailable: $desktopRoot"
}
if (-not (Test-Path -LiteralPath $rustToolsScript -PathType Leaf)) {
    throw "Rust environment script is unavailable: $rustToolsScript"
}

$nodePath = Get-RequiredCommandPath -Name 'node.exe'
$pnpmPath = Get-RequiredCommandPath -Name 'pnpm.cmd'
$null = Get-RequiredCommandPath -Name 'git.exe'

. $rustToolsScript
$rustTools = Initialize-LuminaRustEnvironment
if ($cudaConfiguration.ResolvedAcceleration -eq 'Cuda') {
    Initialize-VisualCppEnvironment
}

$rustFeatures = @(
    'code-mode',
    'local-inference',
    'tui',
    'aws-providers',
    'rustls-tls',
    'system-keyring',
    'disable-update'
)
if ($cudaConfiguration.ResolvedAcceleration -eq 'Cuda') {
    $rustFeatures += 'cuda'
    $env:LUMINA_DESKTOP_CUDA = '1'
} else {
    $env:LUMINA_DESKTOP_CUDA = '0'
}

$rustArguments = @(
    'build',
    '--release',
    '-p',
    'lumina-agent-cli',
    '--bin',
    'lumina',
    '--target-dir',
    $releaseTargetDirectory,
    '--no-default-features',
    '--features',
    ($rustFeatures -join ',')
)

$previousLocation = Get-Location
$buildResult = $null
$previousBuildStampMode = $env:LUMINA_DESKTOP_WRITE_BACKEND_STAMP
try {
    Set-Location -LiteralPath $resolvedRepositoryRoot
    Write-BuildMessage -Message "[1/5] Building Lumina backend with $($cudaConfiguration.ResolvedAcceleration) acceleration."
    Invoke-CheckedCommand -FilePath $rustTools.CargoExe -ArgumentList $rustArguments

    if (-not (Test-Path -LiteralPath $backendBinary -PathType Leaf)) {
        throw "Backend binary was not produced: $backendBinary"
    }
    if (-not (Test-Path -LiteralPath $desktopBinaryDirectory -PathType Container)) {
        $null = New-Item -ItemType Directory -Path $desktopBinaryDirectory -Force
    }
    Copy-Item -LiteralPath $backendBinary -Destination $desktopBinary -Force

    Write-BuildMessage -Message '[2/6] Generating the ACP schema from the same backend source.'
    $schemaFeatures = @(
        'code-mode',
        'local-inference',
        'aws-providers',
        'rustls-tls',
        'system-keyring'
    )
    if ($cudaConfiguration.ResolvedAcceleration -eq 'Cuda') {
        $schemaFeatures += 'cuda'
    }
    $schemaArguments = @(
        'run',
        '--release',
        '--manifest-path',
        'crates/lumina/Cargo.toml',
        '--no-default-features',
        '--features',
        ($schemaFeatures -join ','),
        '--bin',
        'generate-acp-schema',
        '--target-dir',
        $releaseTargetDirectory
    )
    Invoke-CheckedCommand -FilePath $rustTools.CargoExe -ArgumentList $schemaArguments

    Set-Location -LiteralPath $desktopRoot
    Write-BuildMessage -Message '[3/6] Preparing and verifying the backend and optional CUDA runtime.'
    $env:LUMINA_DESKTOP_WRITE_BACKEND_STAMP = '1'
    Invoke-CheckedCommand -FilePath $nodePath -ArgumentList @('scripts/prepare-platform-binaries.js')

    Write-BuildMessage -Message '[4/6] Installing locked desktop dependencies and compiling assets.'
    Invoke-CheckedCommand -FilePath $pnpmPath -ArgumentList @(
        'install',
        '--frozen-lockfile',
        '--config.confirmModulesPurge=false'
    )
    Invoke-CheckedCommand -FilePath $pnpmPath -ArgumentList @('run', 'build-lumina-sdk')
    Invoke-CheckedCommand -FilePath $pnpmPath -ArgumentList @('run', 'i18n:compile')

    Write-BuildMessage -Message '[5/6] Packaging the Lumina desktop application.'
    Invoke-CheckedCommand -FilePath $pnpmPath -ArgumentList @('exec', 'electron-forge', 'package')

    Write-BuildMessage -Message '[6/6] Creating the data-preserving Windows installer.'
    Invoke-CheckedCommand -FilePath $nodePath -ArgumentList @('scripts/build-windows-installer.js')

    if (-not (Test-Path -LiteralPath $packagedApp -PathType Leaf)) {
        throw "Packaged application was not produced: $packagedApp"
    }
    if (-not (Test-Path -LiteralPath $installerDirectory -PathType Container)) {
        throw "Installer directory was not produced: $installerDirectory"
    }

    $buildResult = [pscustomobject]@{
        acceleration = $cudaConfiguration.ResolvedAcceleration
        cudaIncluded = ($cudaConfiguration.ResolvedAcceleration -eq 'Cuda')
        cpuFallback = $true
        packagedApp = $packagedApp
        installerDirectory = $installerDirectory
    }
} finally {
    if ($null -eq $previousBuildStampMode) {
        Remove-Item -Path 'Env:\LUMINA_DESKTOP_WRITE_BACKEND_STAMP' -ErrorAction SilentlyContinue
    } else {
        $env:LUMINA_DESKTOP_WRITE_BACKEND_STAMP = $previousBuildStampMode
    }
    Set-Location -LiteralPath $previousLocation.Path
}

$buildResult | ConvertTo-Json -Compress
