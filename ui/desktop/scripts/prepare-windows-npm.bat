@echo off
setlocal DisableDelayedExpansion
set "SCRIPT_DIR=%~dp0"
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%SCRIPT_DIR%prepare-windows-npm.ps1"
exit /b %ERRORLEVEL%
