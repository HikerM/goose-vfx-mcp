[CmdletBinding()]
param(
    [string]$RepositoryRoot,
    [string]$ApplicationRoot = $env:LUMINA_PATH_ROOT,
    [string]$ReportPath,
    [ValidateSet('cache-install', 'model-assets', 'runtime-assets')][string]$Suite = 'cache-install'
)

function Get-CacheInstallSummary {
    param(
        [Parameter(Mandatory = $true)][string]$Text,
        [Parameter(Mandatory = $true)][string]$Target
    )

    $summaries = @(Get-LifecycleTestResults -Text $Text)
    $expectedIgnored = 0
    $helpers = @{
        legacy_registry = 'legacy_registry_child ... ignored, subprocess helper for legacy registry locking and CAS tests'
        lifecycle_asset_use = 'runtime_asset_guard_child ... ignored, subprocess helper for runtime asset lifetime tests'
        native_runtime_unit = @(
            'lifecycle::native_runtime::process_tests::child_holds_liveness_lock ... ignored, controlled child process for owner lifetime tests',
            'lifecycle::native_runtime::process_tests::owner_parent_holds_child ... ignored, controlled parent process for abrupt-exit job tests'
        )
    }
    if ($helpers.ContainsKey($Target)) {
        $expectedHelpers = @($helpers[$Target])
        $expectedIgnored = $expectedHelpers.Count
        foreach ($helper in $expectedHelpers) {
            $helperPattern = '(?m)^test ' + [regex]::Escape($helper) + '\r?$'
            if ($Text -notmatch $helperPattern) {
                throw (New-RealTextException 'LMCI004' 'The ignored subprocess helper must be identified explicitly.')
            }
        }
    }
    if ($summaries.Count -ne 1 -or $summaries[0].Result -ne 'ok' -or
        $summaries[0].Passed -lt 1 -or $summaries[0].Failed -ne 0 -or $summaries[0].Ignored -ne $expectedIgnored) {
        throw (New-RealTextException 'LMCI002' 'Required tests must execute without failures or unexpected skips.')
    }
    return $summaries[0]
}

function Invoke-CacheInstallValidation {
    param([string]$Root, [string]$ParentRoot, [string]$SelectedSuite = 'cache-install')

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
    $child = Join-Path $parentPath ($SelectedSuite + '-app')
    if (Test-Path -LiteralPath $child) {
        throw (New-RealTextException 'LMRT003' 'Existing cache installation evidence is preserved; use a fresh root.')
    }
    $targets = @('lifecycle_cache_install')
    $scope = 'Cache installation boundary tests; not network transfer or Runtime READY acceptance.'
    if ($SelectedSuite -eq 'model-assets') {
        $targets = @('legacy_registry', 'lifecycle_vision_resources')
        $scope = 'Legacy preservation and bounded vision admission. The ignored subprocess helper is exercised by parent tests, not counted as passed. Real installation and Runtime READY need separate evidence.'
    }
    if ($SelectedSuite -eq 'runtime-assets') {
        $targets = @('runtime_assets_unit', 'native_runtime_unit', 'chat_runtime_unit',
            'lifecycle_asset_use', 'lifecycle_launch_binding', 'lifecycle_native_runtime', 'lifecycle_vector_runtime')
        $scope = 'Runtime asset lifetime, frozen launch binding and vector adapter protocol tests. The subprocess helper is exercised by parent tests, not counted as passed. Real inference and capability READY require separate evidence.'
    }
    $results = @()
    $unitFilters = @{
        runtime_assets_unit = 'runtime_assets::tests'
        native_runtime_unit = 'lifecycle::native_runtime::process_tests'
        chat_runtime_unit = 'lifecycle::chat_runtime::tests'
    }
    foreach ($target in $targets) {
        $selection = @('--test', $target)
        if ($unitFilters.ContainsKey($target)) { $selection = @('--lib', $unitFilters[$target]) }
        $arguments = @('test', '--quiet', '-p', 'lumina-local-inference', '--no-default-features',
            '--offline', '--locked') + $selection + @('--', '--format=pretty')
        $execution = Invoke-LifecycleCargo -Root $rootPath -ChildRoot $child -LogName ($target + '-run.log') -CargoArguments $arguments
        $summary = Get-CacheInstallSummary -Text (Get-RealTextUtf8 -LiteralPath $execution.LogPath) -Target $target
        $results += [pscustomobject]@{
            target = $target; passed = $summary.Passed; failed = $summary.Failed
            ignored = $summary.Ignored; logPath = $execution.LogPath
        }
    }
    return [pscustomobject]@{
        schemaVersion = 1; status = 'PASS'; ruleId = 'LMCI000'; exitCode = 0
        target = ($targets -join ','); targets = $results
        passed = ($results | Measure-Object -Property passed -Sum).Sum
        failed = ($results | Measure-Object -Property failed -Sum).Sum
        ignored = ($results | Measure-Object -Property ignored -Sum).Sum
        logPath = $execution.LogPath; scope = $scope
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
        $result = Invoke-CacheInstallValidation -Root $RepositoryRoot -ParentRoot $ApplicationRoot -SelectedSuite $Suite
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
