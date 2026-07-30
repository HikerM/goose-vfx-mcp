@echo off
setlocal DisableDelayedExpansion

rem Keep this CMD shim limited to human-compatible, non-shell arguments.
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0cargo.ps1" -ValidateCmdShim
if errorlevel 1 exit /b %errorlevel%

powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0cargo.ps1" %*
exit /b %errorlevel%
