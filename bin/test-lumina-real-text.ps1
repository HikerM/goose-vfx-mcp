[CmdletBinding()]
param(
    [string]$RepositoryRoot,

    [string]$ApplicationRoot = $env:LUMINA_PATH_ROOT,

    [switch]$IncludeFoundation,

    [string]$ReportPath
)

function New-RealTextException {
    param([string]$RuleId, [string]$Message)

    $failure = New-Object System.InvalidOperationException($Message)
    $failure.Data['ruleId'] = $RuleId
    return $failure
}

function Get-RealTextEnvironment {
    $names = @('LUMINA_REAL_GGUF_PATH', 'LUMINA_REAL_MODEL_ID', 'LUMINA_REAL_REPO_ID')
    $missing = @($names | Where-Object {
        [string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($_, 'Process'))
    } | Sort-Object)
    return [pscustomobject]@{ Configured = ($missing.Count -eq 0); Missing = $missing }
}

function Get-RealTextAbsolutePath {
    param([Parameter(Mandatory = $true)][string]$LiteralPath)

    if (-not [System.IO.Path]::IsPathRooted($LiteralPath)) {
        throw (New-RealTextException 'LMRT002' 'Validation directories must be absolute.')
    }
    $fullPath = [System.IO.Path]::GetFullPath($LiteralPath)
    if ([System.IO.Path]::GetPathRoot($fullPath) -notmatch '^[DE]:[\\/]$') {
        throw (New-RealTextException 'LMRT002' 'This workspace validation writes only to D: or E:.')
    }
    return $fullPath
}

function Resolve-RealTextDirectory {
    param([Parameter(Mandatory = $true)][string]$LiteralPath)

    $fullPath = Get-RealTextAbsolutePath -LiteralPath $LiteralPath
    $directory = Get-Item -LiteralPath $fullPath -Force
    if (-not $directory.PSIsContainer) {
        throw (New-RealTextException 'LMRT002' 'Validation root is not a directory.')
    }
    for ($ancestor = $directory; $null -ne $ancestor; $ancestor = $ancestor.Parent) {
        if (($ancestor.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw (New-RealTextException 'LMRT002' 'Validation directories cannot contain redirects.')
        }
    }
    return $directory.FullName
}

function Get-RealTextUtf8 {
    param([Parameter(Mandatory = $true)][string]$LiteralPath)

    $stream = [System.IO.File]::OpenRead($LiteralPath)
    try {
        if ($stream.Length -gt 16777216) {
            throw (New-RealTextException 'LMRT005' 'Validation data exceeds its byte limit.')
        }
        $bytes = New-Object byte[] ([int]$stream.Length)
        $offset = 0
        while ($offset -lt $bytes.Length) {
            $count = $stream.Read($bytes, $offset, $bytes.Length - $offset)
            if ($count -eq 0) {
                throw (New-RealTextException 'LMRT005' 'Validation data was truncated during reading.')
            }
            $offset += $count
        }
    } finally {
        $stream.Dispose()
    }
    $decoder = New-Object System.Text.UTF8Encoding($false, $true)
    try {
        return $decoder.GetString($bytes).TrimStart([char]0xFEFF)
    } catch [System.Text.DecoderFallbackException] {
        throw (New-RealTextException 'LMRT005' 'Validation data is not UTF-8.')
    }
}

function Get-LifecycleTestResults {
    param([Parameter(Mandatory = $true)][string]$Text)

    $pattern = '^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;'
    foreach ($line in ($Text -split '\r?\n')) {
        if ($line -match $pattern) {
            [pscustomobject]@{
                Result = $Matches[1]; Passed = [int]$Matches[2]
                Failed = [int]$Matches[3]; Ignored = [int]$Matches[4]
            }
        }
    }
}

function Get-RealTextSha256 {
    param([Parameter(Mandatory = $true)][string]$LiteralPath)

    $stream = [System.IO.File]::OpenRead($LiteralPath)
    $hasher = [System.Security.Cryptography.SHA256]::Create()
    try {
        return ([System.BitConverter]::ToString($hasher.ComputeHash($stream))).Replace('-', '').ToLowerInvariant()
    } finally {
        $hasher.Dispose()
        $stream.Dispose()
    }
}

function Get-RealTextSummary {
    param([Parameter(Mandatory = $true)][string]$Text)

    $summaries = @(Get-LifecycleTestResults -Text $Text)
    if ($summaries.Count -ne 1) {
        throw (New-RealTextException 'LMRT004' 'Actual inference has no unique test result.')
    }
    $summary = $summaries[0]
    if ($summary.Result -ne 'ok' -or $summary.Passed -ne 1 -or
        $summary.Failed -ne 0 -or $summary.Ignored -ne 0) {
        throw (New-RealTextException 'LMRT004' 'Actual inference must execute one passing test without skips.')
    }
    return $summary
}

function Invoke-LifecycleCargo {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$ChildRoot,
        [Parameter(Mandatory = $true)][string]$LogName,
        [Parameter(Mandatory = $true)][string[]]$CargoArguments
    )

    $entry = Join-Path $Root 'bin/cargo.cmd'
    if (-not (Test-Path -LiteralPath $entry -PathType Leaf)) {
        throw (New-RealTextException 'LMRT002' 'The required Windows Cargo entry is missing.')
    }
    [System.IO.Directory]::CreateDirectory($ChildRoot) | Out-Null
    $log = Join-Path $ChildRoot $LogName
    $previousRoot = $env:LUMINA_PATH_ROOT
    Push-Location -LiteralPath $Root
    try {
        $env:LUMINA_PATH_ROOT = $ChildRoot
        $ErrorActionPreference = 'Continue'
        & $entry @CargoArguments 2>&1 | Out-File -LiteralPath $log -Encoding utf8 -ErrorAction Stop
        $testExit = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = 'Stop'
        $env:LUMINA_PATH_ROOT = $previousRoot
        Pop-Location
    }
    if ($testExit -ne 0) {
        throw (New-RealTextException 'LMRT004' ('Cargo validation failed; inspect ' + $log))
    }
    return [pscustomobject]@{ LogPath = $log; ExitCode = $testExit }
}

function Invoke-RealTextCargo {
    param([string]$Root, [string]$ChildRoot)

    $arguments = @('test', '--quiet', '-p', 'lumina-local-inference',
        '--no-default-features', '--offline', '--locked', '--test', 'lifecycle_real_inference',
        'installed_gguf_generates_actual_text', '--', '--ignored', '--nocapture')
    return Invoke-LifecycleCargo -Root $Root -ChildRoot $ChildRoot -LogName 'inference-run.log' -CargoArguments $arguments
}

function Invoke-LifecycleFoundation {
    param([string]$Root, [string]$ParentRoot)

    $child = Join-Path $ParentRoot 'foundation-app'
    if (Test-Path -LiteralPath $child) {
        throw (New-RealTextException 'LMRT003' 'Existing foundation evidence is preserved; use a fresh root.')
    }
    $targets = @('lifecycle_contracts', 'lifecycle_gguf', 'lifecycle_modelscope',
        'lifecycle_revisions', 'lifecycle_classification', 'lifecycle_metadata')
    $arguments = @('test', '--quiet', '-p', 'lumina-local-inference', '--no-default-features', '--offline', '--locked')
    foreach ($target in $targets) { $arguments += @('--test', $target) }
    $execution = Invoke-LifecycleCargo -Root $Root -ChildRoot $child -LogName 'foundation-run.log' -CargoArguments $arguments
    $summaries = @(Get-LifecycleTestResults -Text (Get-RealTextUtf8 -LiteralPath $execution.LogPath))
    $failed = @($summaries | Where-Object { $_.Result -ne 'ok' -or $_.Failed -ne 0 -or $_.Passed -lt 1 })
    if ($summaries.Count -ne $targets.Count -or $failed.Count -gt 0) {
        throw (New-RealTextException 'LMRT004' 'Foundation validation did not execute all expected test binaries.')
    }
    return [pscustomobject]@{
        targets = $targets; suites = $summaries.Count
        passed = ($summaries | Measure-Object -Property Passed -Sum).Sum
        failed = ($summaries | Measure-Object -Property Failed -Sum).Sum
        ignored = ($summaries | Measure-Object -Property Ignored -Sum).Sum
        logPath = $execution.LogPath
    }
}

function Invoke-RealTextValidation {
    param([string]$Root, [string]$ParentRoot, [switch]$Foundation)

    $configuration = Get-RealTextEnvironment
    if (-not $configuration.Configured -or [string]::IsNullOrWhiteSpace($ParentRoot)) {
        $missing = @($configuration.Missing)
        if ([string]::IsNullOrWhiteSpace($ParentRoot)) { $missing += 'LUMINA_PATH_ROOT' }
        return [pscustomobject]@{
            schemaVersion = 1; status = 'UNVERIFIED'; ruleId = 'LMRT001'; exitCode = 2
            missingVariables = @($missing | Sort-Object)
            message = 'Supply actual model variables and an isolated LUMINA_PATH_ROOT.'
        }
    }
    $rootPath = Resolve-RealTextDirectory -LiteralPath $Root
    $parentPath = Resolve-RealTextDirectory -LiteralPath $ParentRoot
    foreach ($temporary in @($env:TEMP, $env:TMP)) {
        Resolve-RealTextDirectory -LiteralPath $temporary | Out-Null
    }
    $childPath = Join-Path $parentPath 'inference-app'
    if (Test-Path -LiteralPath $childPath) {
        throw (New-RealTextException 'LMRT003' 'Use a fresh validation root; existing evidence is preserved.')
    }
    $foundationResult = $null
    if ($Foundation) { $foundationResult = Invoke-LifecycleFoundation -Root $rootPath -ParentRoot $parentPath }
    $execution = Invoke-RealTextCargo -Root $rootPath -ChildRoot $childPath
    $summary = Get-RealTextSummary -Text (Get-RealTextUtf8 -LiteralPath $execution.LogPath)
    $evidencePath = Join-Path $childPath 'evidence/text-smoke.json'
    $evidence = Get-RealTextUtf8 -LiteralPath $evidencePath | ConvertFrom-Json
    if ($evidence.test -ne 'installed_gguf_generates_actual_text') {
        throw (New-RealTextException 'LMRT005' 'Evidence belongs to a different validator.')
    }
    return [pscustomobject]@{
        schemaVersion = 1; status = 'PASS'; ruleId = 'LMRT000'; exitCode = 0
        passed = $summary.Passed; failed = $summary.Failed; ignored = $summary.Ignored
        foundation = $foundationResult
        result = $evidence; evidencePath = $evidencePath; logPath = $execution.LogPath
        scope = 'Actual existing runtime text smoke; not complete lifecycle or release acceptance.'
    }
}

function Resolve-LifecycleReport {
    param([Parameter(Mandatory = $true)][string]$LiteralPath)

    $path = Get-RealTextAbsolutePath -LiteralPath $LiteralPath
    $name = [System.IO.Path]::GetFileName($path)
    if ([string]::IsNullOrWhiteSpace($name) -or $name.IndexOfAny([System.IO.Path]::GetInvalidFileNameChars()) -ge 0) {
        throw (New-RealTextException 'LMRT002' 'Report filename is invalid.')
    }
    $ancestor = [System.IO.Path]::GetDirectoryName($path)
    while (-not (Test-Path -LiteralPath $ancestor)) { $ancestor = [System.IO.Path]::GetDirectoryName($ancestor) }
    Resolve-RealTextDirectory -LiteralPath $ancestor | Out-Null
    if (Test-Path -LiteralPath $path) {
        throw (New-RealTextException 'LMRT003' 'Existing report is preserved; use a fresh report path.')
    }
    return $path
}

function Write-LifecycleReport {
    param([Parameter(Mandatory = $true)][string]$LiteralPath, [Parameter(Mandatory = $true)][object]$Result)

    $lines = @('# lumina model lifecycle validation', '', ('Status: ' + $Result.status),
        'Scope: foundation and actual existing-runtime text smoke; not full product acceptance.', '')
    if ($null -ne $Result.foundation) {
        $lines += @(('Foundation targets: ' + ($Result.foundation.targets -join ', ')),
            ('Foundation passed: ' + $Result.foundation.passed), ('Foundation failed: ' + $Result.foundation.failed),
            ('Foundation ignored: ' + $Result.foundation.ignored), ('Foundation log: ' + $Result.foundation.logPath))
    } else { $lines += 'Foundation: NO_PASS_EVIDENCE' }
    if ($null -ne $Result.result) {
        $lines += @(('Actual text test passed: ' + $Result.passed), ('Actual output: ' + $Result.result.output),
            ('Actual output tokens: ' + $Result.result.outputTokens), ('Inference evidence: ' + $Result.evidencePath),
            ('Inference log: ' + $Result.logPath))
    } else { $lines += 'Actual text: NO_PASS_EVIDENCE' }
    if ($Result.status -ne 'PASS') { $lines += ('Failure or missing configuration: ' + $Result.message) }
    [System.IO.Directory]::CreateDirectory([System.IO.Path]::GetDirectoryName($LiteralPath)) | Out-Null
    $stream = [System.IO.File]::Open($LiteralPath, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write)
    try {
        $encoder = New-Object System.Text.UTF8Encoding($false, $true)
        $bytes = $encoder.GetBytes(($lines -join "`n") + "`n")
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Flush()
    } finally { $stream.Dispose() }
    return [pscustomobject]@{
        path = $LiteralPath; sha256 = (Get-RealTextSha256 -LiteralPath $LiteralPath)
        content = (Get-RealTextUtf8 -LiteralPath $LiteralPath)
    }
}

if ($MyInvocation.InvocationName -ne '.') {
    $ErrorActionPreference = 'Stop'
    $reportTarget = $null
    try {
        if (-not [string]::IsNullOrWhiteSpace($ReportPath)) {
            $reportTarget = Resolve-LifecycleReport -LiteralPath $ReportPath
        }
        if ([string]::IsNullOrWhiteSpace($RepositoryRoot)) {
            $RepositoryRoot = Split-Path -Parent $PSScriptRoot
        }
        $result = Invoke-RealTextValidation -Root $RepositoryRoot -ParentRoot $ApplicationRoot -Foundation:$IncludeFoundation
    } catch {
        $ruleId = 'LMRT900'
        if ($_.Exception.Data.Contains('ruleId')) { $ruleId = $_.Exception.Data['ruleId'] }
        $result = [pscustomobject]@{
            schemaVersion = 1; status = 'FAIL'; ruleId = $ruleId; exitCode = 1
            message = $_.Exception.Message
        }
    }
    if ($null -ne $reportTarget) {
        try {
            $report = Write-LifecycleReport -LiteralPath $reportTarget -Result $result
            $result | Add-Member -MemberType NoteProperty -Name report -Value $report
        } catch {
            $result = [pscustomobject]@{schemaVersion=1;status='FAIL';ruleId='LMRT006';exitCode=1;message=$_.Exception.Message}
        }
    }
    $result | ConvertTo-Json -Depth 8 -Compress | Write-Output
    exit $result.exitCode
}
