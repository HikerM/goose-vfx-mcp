[CmdletBinding()]
param(
    [string]$RepositoryRoot,
    [string]$ApplicationRoot = $env:LUMINA_PATH_ROOT,
    [string]$ReportPath
)

function Get-CacheInstallSummary {
    param([Parameter(Mandatory = $true)][string]$Text)

    $summaries = @(Get-LifecycleTestResults -Text $Text)
    if ($summaries.Count -ne 1 -or $summaries[0].Result -ne 'ok' -or
        $summaries[0].Passed -lt 1 -or $summaries[0].Failed -ne 0 -or $summaries[0].Ignored -ne 0) {
        throw (New-RealTextException 'LMCI002' 'Cache installation tests must execute without failures or skips.')
    }
    return $summaries[0]
}

function Invoke-CacheInstallValidation {
    param([string]$Root, [string]$ParentRoot)

    if ([string]::IsNullOrWhiteSpace($ParentRoot)) {
        return [pscustomobject]@{
            schemaVersion = 1; status = 'UNVERIFIED'; ruleId = 'LMCI001'; exitCode = 2
            message = 'Supply an isolated ApplicationRoot or LUMINA_PATH_ROOT.'
        }
    }
    $rootPath = Resolve-RealTextDirectory -LiteralPath $Root
    $parentPath = Resolve-RealTextDirectory -LiteralPath $ParentRoot
    foreach ($temporary in @($env:TEMP, $env:TMP)) {
        Resolve-RealTextDirectory -LiteralPath $temporary | Out-Null
    }
    $child = Join-Path $parentPath 'cache-install-app'
    if (Test-Path -LiteralPath $child) {
        throw (New-RealTextException 'LMRT003' 'Existing cache installation evidence is preserved; use a fresh root.')
    }
    $arguments = @('test', '--quiet', '-p', 'lumina-local-inference', '--no-default-features',
        '--offline', '--locked', '--test', 'lifecycle_cache_install')
    $execution = Invoke-LifecycleCargo -Root $rootPath -ChildRoot $child -LogName 'cache-install-run.log' -CargoArguments $arguments
    $summary = Get-CacheInstallSummary -Text (Get-RealTextUtf8 -LiteralPath $execution.LogPath)
    return [pscustomobject]@{
        schemaVersion = 1; status = 'PASS'; ruleId = 'LMCI000'; exitCode = 0
        target = 'lifecycle_cache_install'; passed = $summary.Passed; failed = $summary.Failed
        ignored = $summary.Ignored; logPath = $execution.LogPath
        scope = 'Cache installation boundary tests; not network transfer or Runtime READY acceptance.'
    }
}

function Write-CacheInstallReport {
    param([Parameter(Mandatory = $true)][string]$LiteralPath, [Parameter(Mandatory = $true)][object]$Result)

    $content = ($Result | ConvertTo-Json -Depth 8 -Compress) + "`n"
    [System.IO.Directory]::CreateDirectory([System.IO.Path]::GetDirectoryName($LiteralPath)) | Out-Null
    $stream = [System.IO.File]::Open($LiteralPath, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write)
    try {
        $encoder = New-Object System.Text.UTF8Encoding($false, $true)
        $bytes = $encoder.GetBytes($content)
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
        if ([string]::IsNullOrWhiteSpace($RepositoryRoot)) { $RepositoryRoot = Split-Path -Parent $PSScriptRoot }
        $support = Join-Path $RepositoryRoot 'bin/test-lumina-real-text.ps1'
        . $support -RepositoryRoot $RepositoryRoot -ApplicationRoot $ApplicationRoot -ReportPath $ReportPath
        if (-not [string]::IsNullOrWhiteSpace($ReportPath)) { $reportTarget = Resolve-LifecycleReport -LiteralPath $ReportPath }
        $result = Invoke-CacheInstallValidation -Root $RepositoryRoot -ParentRoot $ApplicationRoot
    } catch {
        $ruleId = 'LMCI900'
        if ($_.Exception.Data.Contains('ruleId')) { $ruleId = $_.Exception.Data['ruleId'] }
        $result = [pscustomobject]@{
            schemaVersion = 1; status = 'FAIL'; ruleId = $ruleId; exitCode = 1; message = $_.Exception.Message
        }
    }
    if ($null -ne $reportTarget) {
        try {
            $report = Write-CacheInstallReport -LiteralPath $reportTarget -Result $result
            $result | Add-Member -MemberType NoteProperty -Name report -Value $report
        } catch {
            $result = [pscustomobject]@{schemaVersion=1;status='FAIL';ruleId='LMCI003';exitCode=1;message=$_.Exception.Message}
        }
    }
    $result | ConvertTo-Json -Depth 8 -Compress | Write-Output
    exit $result.exitCode
}
