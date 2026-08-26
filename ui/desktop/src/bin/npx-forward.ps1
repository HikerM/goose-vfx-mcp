$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$ExpectedManifest = @{
  schema = 2
  version = 'v24.18.0'
  url = 'https://nodejs.org/dist/v24.18.0/node-v24.18.0-win-x64.zip'
  sha256 = '0ae68406b42d7725661da979b1403ec9926da205c6770827f33aac9d8f26e821'
}
$ExpectedTopLevel = @{
  'node.exe' = '9a4eb5f1c29c6a2e93852ead46b999e284a6a5ca8bab4d4e241d587d025a52de'
  'npm.cmd' = '21b46c69ad6e2f231f02a9e120f4ba6c8e75fef5a45637103002eab99f888ab8'
  'npx.cmd' = '4dd3574f4396fc3b45c52b6ac80fd52be2dd2660d2a153b4cc807dbbfeefa7a0'
}

function Get-Sha256([string]$Path) {
  $sha = [Security.Cryptography.SHA256]::Create()
  $stream = [IO.File]::OpenRead($Path)
  try { return ([BitConverter]::ToString($sha.ComputeHash($stream))).Replace('-', '').ToLowerInvariant() }
  finally { $stream.Dispose(); $sha.Dispose() }
}

function Assert-RegularTree([string]$Path) {
  $full = [IO.Path]::GetFullPath($Path).TrimEnd('\')
  $root = [IO.Path]::GetPathRoot($full)
  $current = $root.TrimEnd('\')
  foreach ($part in $full.Substring($root.Length).Split('\')) {
    if (!$part) { continue }
    $current = Join-Path $current $part
    if (!(Test-Path -LiteralPath $current)) { throw "Required path is missing: $current" }
    $item = Get-Item -LiteralPath $current -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or !$item.PSIsContainer) {
      throw "Unsafe runtime path: $current"
    }
  }
}

function Assert-Runtime([string]$Node) {
  Assert-RegularTree $env:LUMINA_PATH_ROOT
  Assert-RegularTree $Node
  foreach ($item in Get-ChildItem -LiteralPath $Node -Force -Recurse) {
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Runtime tree contains a reparse point: $($item.FullName)" }
    if ($item.PSIsContainer) { Get-ChildItem -LiteralPath $item.FullName -Force -ErrorAction Stop | Out-Null }
  }
  $manifestPath = Join-Path $Node 'manifest.json'
  if (!(Test-Path -LiteralPath $manifestPath -PathType Leaf)) { throw 'Portable Node manifest is missing; run prepare-windows-npm again' }
  $manifestItem = Get-Item -LiteralPath $manifestPath -Force
  if ($manifestItem.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Portable Node manifest is a reparse point' }
  try { $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json } catch { throw 'Portable Node manifest is invalid JSON; run prepare-windows-npm again' }
  foreach ($key in $ExpectedManifest.Keys) {
    if ($manifest.$key -ne $ExpectedManifest[$key]) { throw 'Portable Node manifest does not match the pinned runtime; run prepare-windows-npm again' }
  }
  if (!$manifest.files) { throw 'Portable Node manifest has no complete file inventory' }

  $expected = @{}
  foreach ($property in $manifest.files.PSObject.Properties) {
    $name = [string]$property.Name
    $parts = $name.Replace('/', '\').Split('\')
    if ([IO.Path]::IsPathRooted($name) -or $name.Contains(':') -or !$name -or ($parts | Where-Object { $_ -eq '' -or $_ -eq '.' -or $_ -eq '..' })) { throw "Unsafe manifest path: $name" }
    $hash = ([string]$property.Value).ToLowerInvariant()
    if ($hash -notmatch '^[0-9a-f]{64}$') { throw "Invalid manifest hash: $name" }
    $expected[$name] = $hash
  }
  foreach ($name in $ExpectedTopLevel.Keys) {
    if (!$expected.ContainsKey($name) -or $expected[$name] -ne $ExpectedTopLevel[$name]) { throw "Pinned runtime file is not in the manifest: $name" }
  }

  $actual = @{}
  foreach ($file in Get-ChildItem -LiteralPath $Node -File -Force -Recurse) {
    if ($file.Name -eq 'manifest.json' -and $file.DirectoryName -eq $Node) { continue }
    if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Runtime file is a reparse point: $($file.FullName)" }
    $relative = $file.FullName.Substring($Node.Length).TrimStart('\').Replace('/', '\')
    $actual[$relative] = $file
  }
  if ($actual.Count -ne $expected.Count) { throw 'Portable Node runtime contains files outside its signed manifest' }
  foreach ($name in $expected.Keys) {
    if (!$actual.ContainsKey($name) -or (Get-Sha256 $actual[$name].FullName) -ne $expected[$name]) {
      throw "Portable Node file failed full-tree integrity validation: $name; run prepare-windows-npm again"
    }
  }
}

Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class LuminaCommandLine {
  [DllImport("shell32.dll", SetLastError=true)] public static extern IntPtr CommandLineToArgvW(IntPtr commandLine, out int argc);
  [DllImport("kernel32.dll")] public static extern IntPtr GetCommandLineW();
  [DllImport("kernel32.dll")] public static extern IntPtr LocalFree(IntPtr hMem);
}
'@
function Read-Arguments([string]$CommandLine) {
  $ptr = [Runtime.InteropServices.Marshal]::StringToHGlobalUni($CommandLine)
  try {
    $count = 0; $argv = [LuminaCommandLine]::CommandLineToArgvW($ptr, [ref]$count)
    if (!$argv -or $count -lt 1) { throw 'Unable to parse the npx command line' }
    try { return @(0..($count - 1) | ForEach-Object { [Runtime.InteropServices.Marshal]::PtrToStringUni([Runtime.InteropServices.Marshal]::ReadIntPtr($argv, $_ * [IntPtr]::Size)) }) }
    finally { [LuminaCommandLine]::LocalFree($argv) | Out-Null }
  } finally { [Runtime.InteropServices.Marshal]::FreeHGlobal($ptr) }
}

$parent = Get-CimInstance Win32_Process -Filter "ProcessId=$PID" | Select-Object -ExpandProperty ParentProcessId
$parentCommand = Get-CimInstance Win32_Process -Filter "ProcessId=$parent" | Select-Object -ExpandProperty CommandLine
$args = Read-Arguments $parentCommand
$selfPath = ([IO.Path]::GetFullPath((Join-Path $PSScriptRoot 'npx.cmd'))).Replace('/', '\')
$index = -1
for ($i = 0; $i -lt $args.Count; $i++) {
  if (([string]$args[$i]).Trim('"').Replace('/', '\') -ieq $selfPath) { $index = $i; break }
}
if ($index -ge 0) {
  $forwarded = @($args | Select-Object -Skip ($index + 1))
} else {
  $selfIndex = $parentCommand.IndexOf($selfPath, [StringComparison]::OrdinalIgnoreCase)
  if ($selfIndex -lt 0) { throw 'Unable to locate npx.cmd in parent command line' }
  $tail = $parentCommand.Substring($selfIndex + $selfPath.Length).Trim().Trim('"')
  $forwarded = if ($tail) { @(Read-Arguments "lumina.exe $tail" | Select-Object -Skip 1) } else { @() }
  if ($tail -and @($forwarded).Count -eq 0) { throw "Unable to restore npx arguments from parent command line: $tail" }
}

$root = if ([string]::IsNullOrWhiteSpace($env:LUMINA_PATH_ROOT)) { 'D:\Lumina' } else { $env:LUMINA_PATH_ROOT }
$root = [IO.Path]::GetFullPath($root).TrimEnd('\')
if ($root -notmatch '^D:\\[^.].*$' -or $root.StartsWith('\\') -or $root -match '[*?"<>|]') { throw 'LUMINA_PATH_ROOT must be a local D: drive folder' }
$env:LUMINA_PATH_ROOT = $root
$nodeDir = Join-Path $root 'runtime\node'
$env:LUMINA_NODE_DIR = $nodeDir

# No directory creation or controlled environment mutation happens before this check.
Assert-Runtime $nodeDir
$node = Join-Path $nodeDir 'node.exe'
$cli = Join-Path $nodeDir 'node_modules\npm\bin\npx-cli.js'
$cache = Join-Path $root 'runtime\npm-cache'
$temp = Join-Path $root 'tmp'
$env:PATH = "$nodeDir;$($env:PATH)"
$env:npm_config_cache = $cache
$env:NPM_CONFIG_CACHE = $cache
$env:TMP = $temp
$env:TEMP = $temp
[IO.Directory]::CreateDirectory($cache) | Out-Null
[IO.Directory]::CreateDirectory($temp) | Out-Null
Assert-RegularTree $cache
Assert-RegularTree $temp
if (!(Test-Path -LiteralPath $cli -PathType Leaf)) { throw 'Portable npm is incomplete; run prepare-windows-npm again' }
& $node $cli @($forwarded)
exit $LASTEXITCODE
