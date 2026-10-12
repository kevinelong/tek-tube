@echo off
rem tek-tube uninstall: Start Menu entries first, then the installed files.
rem
rem   uninstall.cmd            keeps %APPDATA%\tek-tube (shells.txt, presets)
rem   uninstall.cmd -Purge     removes that too
rem
rem The two uninstall files are the last things here on purpose.  cmd.exe reads a
rem batch file block by block as it runs, so a script that deletes the file being
rem read makes the shell give up with "The system cannot find the path specified"
rem half way through - and this file is the one that cannot be deleted from
rem PowerShell.  The shim clears up after itself instead: remove the script, step
rem out of the directory, remove the directory, then let `goto` end this file
rem before `del` removes the file it is reading.
setlocal
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0uninstall.ps1" %*
set "RC=%ERRORLEVEL%"
if exist "%~dp0uninstall.ps1" del /q "%~dp0uninstall.ps1" 2>nul
cd /d "%TEMP%" 2>nul
rem The delete has to be on this line and not before it: `goto` with no label ends
rem this file, so cmd stops reading it, and the rest of the line runs outside the
rem batch file - which is what makes removing this file while it runs possible.
rem With this file gone the directory is empty, and `rd` takes it too.
(goto) 2>nul & del /q "%~f0" 2>nul & rd "%~dp0." 2>nul
exit /b %RC%
