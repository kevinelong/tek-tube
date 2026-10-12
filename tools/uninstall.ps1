<#
.SYNOPSIS
  Removes tek-tube: its Start Menu entries, then the installed files.

.DESCRIPTION
  Deletes only the Start Menu entries that point at *this* installation, so a
  second copy elsewhere keeps its own.  Then it deletes the files this setup
  installed and the directory if that leaves it empty - a directory holding
  anything else is left alone and named, rather than deleted.

  The settings under %APPDATA%\tek-tube (shells.txt and the saved panel presets)
  are the user's own and survive: -Purge removes those too.

.PARAMETER Purge
  Also remove %APPDATA%\tek-tube, i.e. the shell profiles and the saved presets.

.PARAMETER Quiet
  Report only what went wrong.

.EXAMPLE
  .\uninstall.cmd
  .\uninstall.cmd -Purge
#>
[CmdletBinding()]
param(
    [switch]$Purge,
    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'

function Say([string]$message) {
    if (-not $Quiet) { Write-Host $message }
}

$install = $PSScriptRoot
$exe = Join-Path $install 'tek-tube.exe'
$target = $exe
if (Test-Path -LiteralPath $exe) { $target = (Resolve-Path -LiteralPath $exe).Path }

Say ''
Say 'tek-tube - uninstall'
Say ''

# 1. the Start Menu entries, identified by what they point at
$menu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
if (Test-Path -LiteralPath $menu) {
    $shell = New-Object -ComObject WScript.Shell
    Get-ChildItem -LiteralPath $menu -Filter 'tek terminal*.lnk' | ForEach-Object {
        $pointsAt = $shell.CreateShortcut($_.FullName).TargetPath
        if ($pointsAt -and $pointsAt -ieq $target) {
            Remove-Item -LiteralPath $_.FullName -Force
            Say "removed:  $($_.Name)"
        }
    }
}

# 2. the installed files: the ones this setup puts there, and only those.  The
#    uninstaller's own two files are left to the shim (see uninstall.cmd): cmd
#    cannot go on once the batch file it is reading is deleted, and the shell
#    clears them up after this script has returned.
Set-Location $env:TEMP
$files = @(
    'tek-tube.exe', 'README.txt', 'LICENSE.txt', 'NOTICE.txt',
    'THIRD-PARTY-NOTICES.txt'
)
foreach ($file in $files) {
    $path = Join-Path $install $file
    if (Test-Path -LiteralPath $path) {
        try {
            Remove-Item -LiteralPath $path -Force -ErrorAction Stop
            Say "removed:  $file"
        }
        catch {
            Say "left:     $file (in use)"
        }
    }
}
$left = @(Get-ChildItem -LiteralPath $install -Force -ErrorAction SilentlyContinue)
$own = @($left | Where-Object { $_.Name -in @('uninstall.cmd', 'uninstall.ps1') })
if ($left.Count -eq 0) {
    Remove-Item -LiteralPath $install -Force
    Say "removed:  $install"
}
elseif ($left.Count -eq $own.Count) {
    Say "removed:  $install (the uninstaller clears its own last two files)"
}

# 3. the user's settings, which are not ours to delete unless asked
$config = Join-Path $env:APPDATA 'tek-tube'
if ($Purge) {
    if (Test-Path -LiteralPath $config) {
        Remove-Item -LiteralPath $config -Recurse -Force
        Say "purged:   $config (shell profiles and saved presets)"
    }
}
elseif (Test-Path -LiteralPath $config) {
    Say "kept:     $config (--Purge removes it too)"
}

Say ''
