@echo off
powershell.exe -NoLogo -ExecutionPolicy Bypass -File "%~dp0rustup.ps1" %*
exit /b %errorlevel%
