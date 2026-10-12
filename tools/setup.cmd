@echo off
rem tek-tube setup.  Double-click it, or run it from a shell and name the shells
rem you want instead of answering the prompts:
rem
rem   setup.cmd
rem   setup.cmd -Shells "Windows PowerShell","Git Bash" -Default "Git Bash"
rem
rem PowerShell is asked to run the script itself with the execution policy
rem bypassed for this one invocation: Windows blocks .ps1 files by default, and
rem asking anyone to change a machine-wide policy is a worse answer than passing
rem a switch here.
setlocal
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup.ps1" %*
exit /b %ERRORLEVEL%
