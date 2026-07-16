[CmdletBinding()]
param()
$projects = Get-ChildItem -Path @('C:\Users\Public\Documents','D:\') -Filter *.uproject -File -Recurse -ErrorAction SilentlyContinue | Select-Object -Expand FullName
$houdini = Get-ChildItem "$env:USERPROFILE\Documents" -Directory -Filter 'houdini*' -ErrorAction SilentlyContinue | Select-Object -Expand FullName
$nuke = Get-ChildItem 'C:\Program Files' -Directory -Filter 'Nuke*' -ErrorAction SilentlyContinue | Select-Object -Expand FullName
[pscustomobject]@{ UnrealProjects=$projects; HoudiniPreferences=$houdini; NukeInstallations=$nuke; Python=(python --version 2>&1); Uv=(Get-Command uv -ErrorAction SilentlyContinue).Source; Node=(node --version 2>&1); Pnpm=(pnpm --version 2>&1) } | ConvertTo-Json -Depth 3
