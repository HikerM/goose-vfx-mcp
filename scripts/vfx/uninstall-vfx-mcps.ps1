[CmdletBinding(SupportsShouldProcess)] param([switch]$RemoveRuntime)
$root=(Resolve-Path "$PSScriptRoot\..\..").Path; if ($RemoveRuntime -and $PSCmdlet.ShouldProcess((Join-Path $root '.vfx-runtime'),'Remove VFX runtime')) { Remove-Item (Join-Path $root '.vfx-runtime') -Recurse -Force }
