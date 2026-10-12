<#
.SYNOPSIS
  Installs tek-tube and puts a terminal on the Start Menu.

.DESCRIPTION
  Copies tek-tube.exe into a per-user install directory (no administrator is
  needed, nothing goes outside %LOCALAPPDATA% and your own profile), asks which
  shells the Start Menu should offer - the way Windows Terminal offers the shells
  it finds on the machine - and creates a "tek terminal" entry for the default one
  plus one entry per shell.

  The entries pass a shell *name*, not a command line, and the names and commands
  are written to %APPDATA%\tek-tube\shells.txt.  Changing what a shell runs is
  therefore an edit to that one text file, or re-running this setup; nothing has to
  be reinstalled.  `tek-tube.exe --list-shells` prints what is configured.

  Nothing here is installed system-wide and no administrator is needed.  The only
  thing the script runs is `tek-tube.exe --version`, for the version it reports.

.PARAMETER InstallDir
  Where tek-tube.exe is installed.  Default: %LOCALAPPDATA%\Programs\tek-tube.

.PARAMETER Source
  The tek-tube.exe to install.  Default: the one beside this script, or
  ..\target\release\tek-tube.exe when run from a source checkout.

.PARAMETER Shells
  The shells to offer, without prompting, as one comma separated list:
  `-Shells "Windows PowerShell,Git Bash"`.  A name that matches one found on the
  machine is used as it is; "Name=command line" adds a shell of your own.  Names
  with spaces are the ones the Start Menu entries use, so keep them short.

.PARAMETER Default
  Which of them the plain "tek terminal" entry runs.  Default: the first.

.PARAMETER NoShortcuts
  Install and write shells.txt, but create no Start Menu entries.

.PARAMETER Quiet
  Report only what went wrong.

.PARAMETER List
  Print the shells found on this machine, one per line, and stop - what to name
  in -Shells.

.EXAMPLE
  .\setup.cmd

.EXAMPLE
  .\setup.cmd -Shells "Windows PowerShell,Git Bash,WSL (Ubuntu)" -Default "Git Bash"

.EXAMPLE
  .\setup.cmd -InstallDir C:\tools\tek-tube -NoShortcuts -Quiet
#>
[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\tek-tube'),
    [string]$Source,
    [string[]]$Shells,
    [string]$Default,
    [switch]$NoShortcuts,
    [switch]$Quiet,
    [switch]$List
)

$ErrorActionPreference = 'Stop'

function Say([string]$message) {
    if (-not $Quiet) { Write-Host $message }
}

# ---------------------------------------------------------------- the shells

# What Windows Terminal would find on this machine, in the order it would list
# them: the shells that ship with Windows first, then what has been installed.
function Find-Shells {
    $found = New-Object System.Collections.ArrayList
    $system = $env:SystemRoot

    $windowsPowerShell = Join-Path $system 'System32\WindowsPowerShell\v1.0\powershell.exe'
    if (Test-Path -LiteralPath $windowsPowerShell) {
        [void]$found.Add([pscustomobject]@{
            Name = 'Windows PowerShell'; Command = $windowsPowerShell
        })
    }

    $pwsh = Get-Command pwsh.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($pwsh) {
        [void]$found.Add([pscustomobject]@{ Name = 'PowerShell'; Command = $pwsh.Source })
    }

    $cmd = Join-Path $system 'System32\cmd.exe'
    if (Test-Path -LiteralPath $cmd) {
        [void]$found.Add([pscustomobject]@{ Name = 'Command Prompt'; Command = $cmd })
    }

    # Git Bash: quoted, because Git installs under "Program Files" by default and
    # this is a command line, not a path.
    $bashCandidates = @()
    if ($env:ProgramFiles) { $bashCandidates += (Join-Path $env:ProgramFiles 'Git\bin\bash.exe') }
    if (${env:ProgramFiles(x86)}) { $bashCandidates += (Join-Path ${env:ProgramFiles(x86)} 'Git\bin\bash.exe') }
    if ($env:LOCALAPPDATA) { $bashCandidates += (Join-Path $env:LOCALAPPDATA 'Programs\Git\bin\bash.exe') }
    foreach ($bash in $bashCandidates) {
        if (Test-Path -LiteralPath $bash) {
            [void]$found.Add([pscustomobject]@{ Name = 'Git Bash'; Command = '"' + $bash + '" --login -i' })
            break
        }
    }

    # One profile per WSL distribution, as Windows Terminal does.  `wsl -l -q`
    # writes UTF-16 and PowerShell 5.1 decodes it one byte per character, so every
    # letter arrives followed by a NUL: strip them all before splitting, or the
    # NULs travel into the profile name, into the path of a Start Menu entry and
    # into the command in shells.txt.  (Found the hard way - WScript.Shell refuses
    # a shortcut path containing a NUL with "The shortcut pathname must end with
    # .lnk or .url", which says nothing about NULs, and the name looks perfect
    # everywhere it is printed.)
    $wsl = Get-Command wsl.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($wsl) {
        $distros = ((@(& $wsl.Source -l -q 2>$null) -join "`n") -replace "`0", '') -split "`r?`n" |
            ForEach-Object { $_.Trim() } |
            Where-Object { $_ }
        foreach ($distro in $distros) {
            [void]$found.Add([pscustomobject]@{
                Name = "WSL ($distro)"; Command = "wsl.exe -d `"$distro`""
            })
        }
    }

    return $found
}

# The shells to offer: the ones named on the command line, or the ones picked at
# the prompt.  Returns a list of [Name, Command] pairs.
function Choose-Shells($found) {
    # -Shells arrives as an array when this is called from PowerShell
    # (-Shells 'a','b') and as one comma separated string when setup.cmd is run from
    # cmd.exe, which does not split it.  Both are accepted, because setup.cmd is what
    # most people run: `-Shells "Windows PowerShell,Git Bash"` works either way.
    $wanted = @($Shells |
        ForEach-Object { $_ -split ',' } |
        ForEach-Object { $_.Trim() } |
        Where-Object { $_ })
    if ($wanted.Count) {
        $chosen = New-Object System.Collections.ArrayList
        foreach ($name in $wanted) {
            if ($name -match '=') {
                # "Name=command line": a shell of your own, no detection involved
                $parts = $name -split '=', 2
                [void]$chosen.Add([pscustomobject]@{
                    Name = $parts[0].Trim(); Command = $parts[1].Trim()
                })
                continue
            }
            $match = $found | Where-Object { $_.Name -ieq $name } | Select-Object -First 1
            if (-not $match) {
                $available = ($found | ForEach-Object { $_.Name }) -join ', '
                throw "no shell called '$name' was found.  Found: $available"
            }
            [void]$chosen.Add($match)
        }
        if ($chosen.Count -eq 0) { throw 'no shells were named' }
        return $chosen
    }

    Write-Host ''
    Write-Host 'Which shells should the Start Menu offer?'
    Write-Host ''
    for ($i = 0; $i -lt $found.Count; $i++) {
        Write-Host ('  [{0}] {1,-20} {2}' -f ($i + 1), $found[$i].Name, $found[$i].Command)
    }
    $custom = $found.Count + 1
    Write-Host ("  [{0}] a shell of your own - you type the command" -f $custom)
    Write-Host ''
    $answer = Read-Host ('Numbers, separated by spaces, for the shells to offer ([Enter] = all of them)')

    $picked = New-Object System.Collections.ArrayList
    if ([string]::IsNullOrWhiteSpace($answer)) {
        foreach ($shell in $found) { [void]$picked.Add($shell) }
    }
    else {
        foreach ($field in ($answer -split '[,\s]+' | Where-Object { $_ })) {
            $number = 0
            if (-not [int]::TryParse($field, [ref]$number) -or $number -lt 1 -or $number -gt $custom) {
                throw "'$field' is not one of the numbers above"
            }
            if ($number -eq $custom) {
                $name = Read-Host 'Call it what?'
                $command = Read-Host 'Command line to start it'
                [void]$picked.Add([pscustomobject]@{
                    Name = $name.Trim(); Command = $command.Trim()
                })
            }
            else {
                [void]$picked.Add($found[$number - 1])
            }
        }
    }
    if ($picked.Count -eq 0) { throw 'no shells were chosen' }

    Write-Host ''
    Write-Host 'Which of them should the plain "tek terminal" entry run?'
    for ($i = 0; $i -lt $picked.Count; $i++) {
        Write-Host ('  [{0}] {1}' -f ($i + 1), $picked[$i].Name)
    }
    $answer = Read-Host ("Number ([Enter] = 1, {0})" -f $picked[0].Name)
    if ([string]::IsNullOrWhiteSpace($answer)) {
        $Script:ChosenDefault = $picked[0].Name
    }
    else {
        $number = 0
        if (-not [int]::TryParse($answer.Trim(), [ref]$number) -or $number -lt 1 -or $number -gt $picked.Count) {
            throw "'$answer' is not one of the numbers above"
        }
        $Script:ChosenDefault = $picked[$number - 1].Name
    }
    return $picked
}

# ------------------------------------------------------------- the shortcuts

function New-TekShortcut([string]$path, [string]$target, [string]$arguments, [string]$description) {
    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut($path)
    $link.TargetPath = $target
    if ($arguments) { $link.Arguments = $arguments }
    # A shell starts where Windows Terminal starts it: the user's own directory.
    $link.WorkingDirectory = [Environment]::GetFolderPath('UserProfile')
    $link.Description = $description
    $link.IconLocation = "$target,0"
    $link.Save()
}

function Install-Shortcuts([string]$target, $shells, [string]$defaultName) {
    $menu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
    # It is there on any real profile; creating it keeps a fresh or redirected
    # %APPDATA% from turning the install into a failure.
    if (-not (Test-Path -LiteralPath $menu)) {
        New-Item -ItemType Directory -Path $menu -Force | Out-Null
    }

    # The default entry passes no --shell, so it follows shells.txt: changing the
    # default in that file moves this entry too, without re-running anything.
    $written = @()
    $plain = Join-Path $menu 'tek terminal.lnk'
    New-TekShortcut $plain $target '' "tek-tube: $defaultName on a storage tube"
    $written += $plain

    foreach ($shell in $shells) {
        if ($shell.Name -ieq $defaultName) { continue }
        # Shortcut names cannot contain \ / : * ? " < > |, and a control character
        # is not a name either - a name can come from a shell's own output.
        $safe = ($shell.Name -replace '[\\/:*?"<>|]', '-') -replace '[\x00-\x1f]', ''
        $path = Join-Path $menu ("tek terminal ({0}).lnk" -f $safe)
        New-TekShortcut $path $target ("--shell `"{0}`"" -f $shell.Name) "tek-tube: $($shell.Name) on a storage tube"
        $written += $path
    }
    return $written
}

# ------------------------------------------------------------------ the work

# -List is what -Shells is written against: print what was found and stop, so a
# scripted install can name the shells this machine actually has.
if ($List) {
    foreach ($shell in @(Find-Shells)) {
        Write-Host ('{0,-20} {1}' -f $shell.Name, $shell.Command)
    }
    return
}

$exe = $Source
if (-not $exe) {
    $beside = Join-Path $PSScriptRoot 'tek-tube.exe'
    $fromSource = Join-Path $PSScriptRoot '..\target\release\tek-tube.exe'
    if (Test-Path -LiteralPath $beside) { $exe = $beside }
    elseif (Test-Path -LiteralPath $fromSource) { $exe = (Resolve-Path -LiteralPath $fromSource).Path }
    else { throw "no tek-tube.exe beside this script: pass -Source <path to tek-tube.exe>" }
}
$exe = (Resolve-Path -LiteralPath $exe).Path
# The version comes from the binary itself, so the banner, the shortcuts and the
# files all belong to the same build.  The only thing this script runs is
# `tek-tube.exe --version`.
$version = (& $exe --version 2>$null) -replace '^tek-tube\s+', ''
if (-not $version) { $version = 'unknown' }

Say ''
Say "tek-tube $version - setup"
Say ''

$found = @(Find-Shells)
if ($found.Count -eq 0) { throw 'no shells were found on this machine' }
$chosen = @(Choose-Shells $found)

$defaultName = $Default
if (-not $defaultName) {
    if ($Script:ChosenDefault) { $defaultName = $Script:ChosenDefault }
    else { $defaultName = $chosen[0].Name }
}
$defaultShell = $chosen | Where-Object { $_.Name -ieq $defaultName } | Select-Object -First 1
if (-not $defaultShell) { throw "'$defaultName' is not one of the shells being offered" }

# 1. the program, and the text that has to travel with it
if (-not (Test-Path -LiteralPath $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}
Copy-Item -LiteralPath $exe -Destination (Join-Path $InstallDir 'tek-tube.exe') -Force
foreach ($text in @('README.txt', 'LICENSE.txt', 'NOTICE.txt', 'THIRD-PARTY-NOTICES.txt')) {
    $from = Join-Path $PSScriptRoot $text
    if (Test-Path -LiteralPath $from) {
        Copy-Item -LiteralPath $from -Destination (Join-Path $InstallDir $text) -Force
    }
}
foreach ($script in @('uninstall.cmd', 'uninstall.ps1')) {
    $from = Join-Path $PSScriptRoot $script
    if (Test-Path -LiteralPath $from) {
        Copy-Item -LiteralPath $from -Destination (Join-Path $InstallDir $script) -Force
    }
}
Say "installed:  $(Join-Path $InstallDir 'tek-tube.exe')"

# 2. the shells the Start Menu entries name
$configDir = Join-Path $env:APPDATA 'tek-tube'
if (-not (Test-Path -LiteralPath $configDir)) {
    New-Item -ItemType Directory -Path $configDir -Force | Out-Null
}
$configPath = Join-Path $configDir 'shells.txt'
$lines = New-Object System.Collections.ArrayList
[void]$lines.Add('# tek-tube shells: one per line, `name = command line`:')
[void]$lines.Add('#   the Start Menu entries pass the name, so a command can be changed here')
[void]$lines.Add('#   without touching any shortcut.  `default` is what a bare tek-tube.exe runs.')
[void]$lines.Add('')
[void]$lines.Add("default = $($defaultShell.Name)")
foreach ($shell in $chosen) {
    [void]$lines.Add("$($shell.Name) = $($shell.Command)")
}
# no BOM: the file is read by the program, not only by PowerShell
[System.IO.File]::WriteAllText($configPath, (($lines -join "`r`n") + "`r`n"), (New-Object System.Text.UTF8Encoding($false)))
Say "shells:     $configPath"

# 3. the Start Menu
if ($NoShortcuts) {
    Say 'shortcuts:  none (--NoShortcuts)'
}
else {
    $written = Install-Shortcuts (Join-Path $InstallDir 'tek-tube.exe') $chosen $defaultShell.Name
    Say ("shortcuts:  {0}" -f ($written | ForEach-Object { Split-Path -Leaf $_ }) -join ', ')
}

Say ''
Say "Start Menu:  tek terminal            runs $($defaultShell.Name)"
foreach ($shell in $chosen) {
    if ($shell.Name -ieq $defaultShell.Name) { continue }
    Say ("             tek terminal ({0})" -f $shell.Name)
}
Say ''
Say "Check it:    $(Join-Path $InstallDir 'tek-tube.exe') --list-shells"
Say "Change it:   run setup again, or edit $configPath"
if (-not $NoShortcuts) {
    Say "Remove it:   $(Join-Path $InstallDir 'uninstall.cmd')"
}
Say ''
