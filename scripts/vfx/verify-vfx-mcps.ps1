[CmdletBinding()] param()
$root=(Resolve-Path "$PSScriptRoot\..\..").Path; Get-Content (Join-Path $root 'integrations\vfx\manifest.json') -Raw | ConvertFrom-Json | Out-Null; Write-Host 'Manifest JSON is valid. Runtime DCC connection tests require the corresponding editor to be running and are skipped by default.'
