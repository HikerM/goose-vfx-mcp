$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Version = 'v24.18.0'
$Url = 'https://nodejs.org/dist/v24.18.0/node-v24.18.0-win-x64.zip'
$Sha256 = '0ae68406b42d7725661da979b1403ec9926da205c6770827f33aac9d8f26e821'
$ExpectedHashes = @{
  'node.exe' = '9a4eb5f1c29c6a2e93852ead46b999e284a6a5ca8bab4d4e241d587d025a52de'
  'npm.cmd' = '21b46c69ad6e2f231f02a9e120f4ba6c8e75fef5a45637103002eab99f888ab8'
  'npx.cmd' = '4dd3574f4396fc3b45c52b6ac80fd52be2dd2660d2a153b4cc807dbbfeefa7a0'
}
$null = Add-Type -AssemblyName System.Net.Http
$Root = if ($env:LUMINA_PATH_ROOT) { $env:LUMINA_PATH_ROOT } else { 'D:\Lumina' }
$Root = $Root.Replace('/', '\').TrimEnd('\')

function Assert-DPath([string]$Path) {
  if ($Path -notmatch '^D:\\[^.].*$' -or $Path.StartsWith('\\') -or $Path -match '[*?"<>|]' ) { throw "LUMINA_PATH_ROOT must be a local D: drive folder: $Path" }
  foreach ($part in $Path.Substring(3).Split('\')) {
    if (!$part -or $part -in '.', '..' -or $part.EndsWith('.') -or $part.EndsWith(' ') -or $part.Contains(':')) { throw "Unsafe D: path segment: $part" }
  }
}
function Get-Sha256([string]$Path) {
  $sha = [Security.Cryptography.SHA256]::Create()
  $stream = [IO.File]::OpenRead($Path)
  try { return ([BitConverter]::ToString($sha.ComputeHash($stream))).Replace('-', '').ToLowerInvariant() }
  finally { $stream.Dispose(); $sha.Dispose() }
}
function Assert-RegularTree([string]$Path) {
  $current = 'D:\'
  foreach ($part in $Path.Substring(3).Split('\')) {
    $current = Join-Path $current $part
    if (Test-Path -LiteralPath $current) {
      $item = Get-Item -LiteralPath $current -Force
      if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Reparse point rejected: $current" }
      if (!$item.PSIsContainer) { throw "Not a directory: $current" }
      $current = $item.FullName.TrimEnd('\')
    } else { break }
  }
}
function Assert-NoReparseDescendants([string]$Path) {
  $pending = [Collections.Generic.Stack[string]]::new(); $pending.Push($Path)
  while ($pending.Count -gt 0) {
    $dir = $pending.Pop()
    $item = Get-Item -LiteralPath $dir -Force -ErrorAction Stop
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Reparse point rejected: $dir" }
    foreach ($child in Get-ChildItem -LiteralPath $dir -Force -ErrorAction Stop) {
      if ($child.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Reparse point rejected: $($child.FullName)" }
      if ($child.PSIsContainer) { $pending.Push($child.FullName) }
    }
  }
}
function Assert-Manifest([string]$Node) {
  if (!(Test-Path -LiteralPath $Node -PathType Container)) { return $false }
  try { Assert-RegularTree $Node; Assert-NoReparseDescendants $Node } catch { return $false }
  $manifestPath = Join-Path $Node 'manifest.json'
  if (!(Test-Path -LiteralPath $manifestPath -PathType Leaf)) { return $false }
  try { $m = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json } catch { return $false }
  if ($m.schema -ne 2 -or $m.version -ne $Version -or $m.url -ne $Url -or $m.sha256 -ne $Sha256 -or !$m.files) { return $false }
  $names = @($m.files.PSObject.Properties.Name)
  $actual = @(Get-ChildItem -LiteralPath $Node -File -Force -Recurse | Where-Object { !($_.Name -eq 'manifest.json' -and $_.DirectoryName -eq $Node) })
  if ($names.Count -ne $actual.Count) { return $false }
  foreach ($file in $actual) {
    $relative = $file.FullName.Substring($Node.Length).TrimStart('\').Replace('/', '\')
    if (!$m.files.$relative -or (Get-Sha256 $file.FullName) -ne ([string]$m.files.$relative).ToLowerInvariant()) { return $false }
  }
  foreach ($name in @('node.exe','npm.cmd','npx.cmd')) {
    $file = Join-Path $Node $name
    if (!(Test-Path -LiteralPath $file -PathType Leaf)) { return $false }
    $item = Get-Item -LiteralPath $file -Force
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { return $false }
    $expected = $m.files.$name
    if (!$expected -or $expected.ToLowerInvariant() -ne $ExpectedHashes[$name] -or (Get-Sha256 $file) -ne $ExpectedHashes[$name]) { return $false }
  }
  return $true
}

Assert-DPath $Root
Assert-RegularTree $Root
$runtime = Join-Path $Root 'runtime\node'
$runtimeParent = Join-Path $Root 'runtime'
$stage = Join-Path $Root 'runtime\.node-staging'
$lockPath = Join-Path $Root 'runtime\.node-install.lock'
$zip = Join-Path $Root 'runtime\.node-download.zip'
New-Item -ItemType Directory -Path $runtimeParent -Force | Out-Null
Assert-RegularTree $runtimeParent
$lockToken = [guid]::NewGuid().ToString('N')
$lockDeadline = [DateTime]::UtcNow.AddSeconds(30)
while ($true) {
  try {
    $lock = [IO.File]::Open($lockPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    $writer = [IO.StreamWriter]::new($lock); $writer.Write($lockToken); $writer.Flush(); $writer.Dispose(); $lock.Position = 0
    $reader = [IO.StreamReader]::new($lock, [Text.Encoding]::UTF8, $false, 64, $true); $observed = $reader.ReadToEnd(); $reader.Dispose()
    if ($observed -ne $lockToken -or ([IO.File]::GetAttributes($lockPath) -band [IO.FileAttributes]::ReparsePoint)) { throw 'Installer lock identity verification failed' }
    break
  } catch [IO.IOException] {
    if ([DateTime]::UtcNow -ge $lockDeadline) { throw 'Installer lock acquisition timed out' }
    Start-Sleep -Milliseconds 100
  }
}
try {
  Assert-RegularTree $runtimeParent
  if (Assert-Manifest $runtime) { & (Join-Path $runtime 'node.exe') '--version'; exit $LASTEXITCODE }
  $previous = Join-Path $runtimeParent '.node-previous'
  if (Test-Path -LiteralPath $previous) {
    if (Assert-Manifest $previous) {
      if (Test-Path -LiteralPath $runtime) {
        Assert-RegularTree $runtime
        Remove-Item -LiteralPath $runtime -Recurse -Force
      }
      Move-Item -LiteralPath $previous -Destination $runtime
      if (Assert-Manifest $runtime) { Write-Output 'Recovered the previous complete Node runtime'; exit 0 }
      throw 'Runtime recovery produced an unverifiable installation'
    }
    Assert-RegularTree $previous
    Remove-Item -LiteralPath $previous -Recurse -Force
  }
  if (Test-Path -LiteralPath $stage) { Assert-RegularTree $stage; Remove-Item -LiteralPath $stage -Recurse -Force }
  if (Test-Path -LiteralPath $zip) { if ((Get-Item -LiteralPath $zip -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Node download artifact is a reparse point' }; Remove-Item -LiteralPath $zip -Force }
  $client = [System.Net.Http.HttpClient]::new(); $client.Timeout = [TimeSpan]::FromMinutes(10)
  try { $bytes = $client.GetByteArrayAsync($Url).GetAwaiter().GetResult(); [IO.File]::WriteAllBytes($zip, $bytes) } catch { throw "Node download failed from ${Url}: $($_.Exception.Message)" } finally { $client.Dispose() }
  if ((Get-Sha256 $zip) -ne $Sha256) { throw 'Node archive SHA-256 mismatch; refusing to extract' }
  Expand-Archive -LiteralPath $zip -DestinationPath $stage -Force
  $extracted = Join-Path $stage "node-v24.18.0-win-x64"
  if (!(Test-Path -LiteralPath $extracted -PathType Container)) { throw 'Node archive has an unexpected layout' }
  Assert-RegularTree $stage
  Assert-NoReparseDescendants $stage
  $files = @{}
  foreach ($name in @('node.exe','npm.cmd','npx.cmd')) {
    $source = Join-Path $extracted $name
    if (!(Test-Path -LiteralPath $source -PathType Leaf)) { throw "Node archive is missing $name" }
    $files[$name] = Get-Sha256 $source
  }
$inventory = [ordered]@{}
foreach ($file in Get-ChildItem -LiteralPath $extracted -File -Force -Recurse) {
  $relative = $file.FullName.Substring($extracted.Length).TrimStart('\').Replace('/', '\')
  if ($relative -eq 'manifest.json') { continue }
  if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Node archive contains a reparse point: $relative" }
  $inventory[$relative] = Get-Sha256 $file.FullName
}
$manifest = [ordered]@{ schema = 2; version = $Version; url = $Url; sha256 = $Sha256; files = $inventory; installedAtUtc = [DateTime]::UtcNow.ToString('o') }
  $manifest | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $extracted 'manifest.json') -Encoding UTF8
  $previous = Join-Path $runtimeParent '.node-previous'
  if (Test-Path -LiteralPath $runtime) {
    Assert-RegularTree $runtime
    Move-Item -LiteralPath $runtime -Destination $previous -Force
  }
  try {
    Move-Item -LiteralPath $extracted -Destination $runtime
    if (!(Assert-Manifest $runtime)) { throw 'Installed Node manifest verification failed' }
  } catch {
    if (!(Test-Path -LiteralPath $runtime) -and (Assert-Manifest $previous)) {
      Move-Item -LiteralPath $previous -Destination $runtime -Force
    }
    throw
  }
  if (Test-Path -LiteralPath $previous) { Remove-Item -LiteralPath $previous -Recurse -Force }
  Remove-Item -LiteralPath $stage -Recurse -Force
  Remove-Item -LiteralPath $zip -Force
  & (Join-Path $runtime 'node.exe') '--version'
  if ($LASTEXITCODE -ne 0) { throw "Installed node.exe --version failed with exit code $LASTEXITCODE" }
} finally {
  if ($lock) {
    $lock.Dispose()
    try {
      $attrs = [IO.File]::GetAttributes($lockPath)
      if (!($attrs -band [IO.FileAttributes]::ReparsePoint) -and [IO.File]::ReadAllText($lockPath) -eq $lockToken) { [IO.File]::Delete($lockPath) }
    } catch { }
  }
}
