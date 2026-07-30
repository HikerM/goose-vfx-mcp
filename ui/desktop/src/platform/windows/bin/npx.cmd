@ECHO OFF
SETLOCAL DisableDelayedExpansion

REM The forwarder obtains the original command line through CommandLineToArgvW.
REM Keep this launcher free of PowerShell -Command and of %* interpolation.
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%~dp0npx-forward.ps1"
EXIT /B %ERRORLEVEL%
