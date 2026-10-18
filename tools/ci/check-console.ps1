<#
.SYNOPSIS
  Windows only: checks the console behaviour of the windows-subsystem release exe (DESIGN 14.3, A-051, C-017).

.DESCRIPTION
  1. A normal launch (no flags) neither allocates nor attaches a console: after the exe has opened its
     window, AttachConsole(<its pid>) fails with ERROR_INVALID_HANDLE. Checked for `cmd.exe /c "<exe>"`
     (a parent that owns a console and passes no pipes on: the shape that used to attach one) and, with
     -ShellExecute or on CI, for ShellExecute (what Explorer does). The probe runs in a child pwsh with
     redirected output, so this script's own console is never detached. The exe is stopped again.
  2. Headless output reaches a redirect in every shape a user can type, through cmd.exe, whose own
     console stays the exe's stderr when it is not redirected (the shape that used to lose the output):
       exe --help > f      exe --version > f      exe --help > f 2>&1      exe --version | more > f
  Isolation (DESIGN 15.4): the sandbox variable set of tools/qa/run-gui.ps1 is applied to THIS process
  for the duration of the run (ShellExecute cannot take an environment) and restored afterwards.
  -ProfileDir must hold a .qa-sandbox marker (the default, a fresh temp dir, gets one) and must not be
  the real profile directory. The exe is run from a uniquely named copy; only that name is ever stopped.

.EXAMPLE
  ./tools/ci/check-console.ps1 -Exe target/release/claude-history-gui.exe
#>
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [string]$ProfileDir = (Join-Path ([System.IO.Path]::GetTempPath()) ('chg-console-' + [guid]::NewGuid().ToString('N'))),
    [switch]$ShellExecute
)
$ErrorActionPreference = 'Stop'
$exe = (Resolve-Path -LiteralPath $Exe).Path
$ProfileDir = [System.IO.Path]::GetFullPath($ProfileDir)
$realProfile = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
if ($realProfile -and [string]::Equals($ProfileDir.TrimEnd('\', '/'), $realProfile.TrimEnd('\', '/'), [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'Refusing: -ProfileDir is the real profile directory.'
}
New-Item -ItemType Directory -Force -Path (Join-Path $ProfileDir '.claude') | Out-Null
if (-not (Test-Path -LiteralPath (Join-Path $ProfileDir '.qa-sandbox'))) {
    if (Get-ChildItem -LiteralPath $ProfileDir -Force | Where-Object { $_.Name -ne '.claude' }) {
        throw "'$ProfileDir' has no .qa-sandbox marker and is not empty; refusing."
    }
    New-Item -ItemType File -Force -Path (Join-Path $ProfileDir '.qa-sandbox') | Out-Null
}
$work = Join-Path $ProfileDir 'out'
New-Item -ItemType Directory -Force -Path $work | Out-Null

# The sandbox environment (mirrors tools/qa/run-gui.ps1), applied to this process and restored at the end.
$sep = [System.IO.Path]::DirectorySeparatorChar
$sandboxEnv = [ordered]@{
    'USERPROFILE'         = $ProfileDir
    'HOME'                = $ProfileDir
    'CLAUDE_CONFIG_DIR'   = (Join-Path $ProfileDir '.claude')
    'PI_CODING_AGENT_DIR' = (Join-Path $ProfileDir ".pi${sep}agent")
    'XDG_DATA_HOME'       = (Join-Path $ProfileDir ".local${sep}share")
    'XDG_CONFIG_HOME'     = (Join-Path $ProfileDir '.config')
    'XDG_CACHE_HOME'      = (Join-Path $ProfileDir '.cache')
    'XDG_STATE_HOME'      = (Join-Path $ProfileDir ".local${sep}state")
}
$removeEnv = 'PI_CODING_AGENT_SESSION_DIR', 'PI_CONFIG_DIR', 'OMP_PROFILE', 'PI_PROFILE', 'TERMINAL', 'CHG_ALLOW_REAL_HOME', 'CHG_ALLOW_REAL_LAUNCH', 'CLAUDE_HISTORY_CLIPBOARD'
$saved = @{}
foreach ($n in @($sandboxEnv.Keys) + $removeEnv) { $saved[$n] = [Environment]::GetEnvironmentVariable($n, 'Process') }
foreach ($n in $sandboxEnv.Keys) { [Environment]::SetEnvironmentVariable($n, $sandboxEnv[$n], 'Process') }
foreach ($n in $removeEnv) { [Environment]::SetEnvironmentVariable($n, $null, 'Process') }

# The probe: detach from its own console, then try to attach to the exe's. It runs in a child pwsh whose
# output is redirected, so the console state of this script never changes.
$probe = Join-Path $work 'probe.ps1'
Set-Content -LiteralPath $probe -Encoding utf8 -Value @'
param([uint32]$TargetPid)
Add-Type -Namespace Chg -Name Con -MemberDefinition @"
[DllImport("kernel32.dll", SetLastError=true)] public static extern bool FreeConsole();
[DllImport("kernel32.dll", SetLastError=true)] public static extern bool AttachConsole(uint pid);
"@
[void][Chg.Con]::FreeConsole()
$ok = [Chg.Con]::AttachConsole($TargetPid)
$err = [System.Runtime.InteropServices.Marshal]::GetLastWin32Error()
if ($ok) { [void][Chg.Con]::FreeConsole(); [Console]::Out.WriteLine('attached') } else { [Console]::Out.WriteLine("error $err") }
'@
$shellExe = (Get-Process -Id $PID).Path

function Get-AttachResult([int]$targetPid) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo $shellExe
    $psi.Arguments = "-NoProfile -File `"$probe`" $targetPid"
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $p = [System.Diagnostics.Process]::Start($psi)
    $out = $p.StandardOutput.ReadToEnd().Trim()
    $p.WaitForExit()
    return $out
}

# 1. A normal launch has no console. The CreateProcess case goes through cmd.exe (which owns a hidden
#    console and passes no pipes on), the ShellExecute case is launched directly; this script never
#    calls FreeConsole itself.
function Test-NoConsole([string]$label, [bool]$shell) {
    $name = 'chg-nc-' + [guid]::NewGuid().ToString('N').Substring(0, 12)
    $copy = Join-Path $work "$name.exe"
    Copy-Item -LiteralPath $exe -Destination $copy
    try {
        if ($shell) {
            $psi = New-Object System.Diagnostics.ProcessStartInfo $copy
            $psi.UseShellExecute = $true
            [void][System.Diagnostics.Process]::Start($psi)
        } else {
            $psi = New-Object System.Diagnostics.ProcessStartInfo 'cmd.exe'
            $psi.Arguments = '/c "' + $copy + '"'
            $psi.UseShellExecute = $false
            $psi.CreateNoWindow = $true
            # cmd.exe /c waits for the GUI exe: do not wait; the exe is stopped below and cmd ends with it.
            [void][System.Diagnostics.Process]::Start($psi)
        }
        Start-Sleep -Seconds 6
        $procs = @(Get-Process -Name $name -ErrorAction SilentlyContinue)
        if ($procs.Count -eq 0) { return "$label : the exe is not running (exited early?)" }
        $res = Get-AttachResult $procs[0].Id
        if ($res -eq 'attached') { return "$label : the exe owns a console" }
        if ($res -ne 'error 6') { return "$label : AttachConsole gave '$res', expected 'error 6' (ERROR_INVALID_HANDLE)" }
        Write-Host "ok   $label : no console (AttachConsole(pid) = ERROR_INVALID_HANDLE)"
        return $null
    } finally {
        Get-Process -Name $name -ErrorAction SilentlyContinue | Stop-Process -Force
        Start-Sleep -Milliseconds 300
        Remove-Item -LiteralPath $copy -Force -ErrorAction SilentlyContinue
    }
}

# 2. Headless output reaches the redirect. cmd.exe does not wait for a GUI-subsystem process: poll.
function Test-Redirect([string]$label, [string]$commandTail, [string]$pattern) {
    $file = Join-Path $work ([guid]::NewGuid().ToString('N') + '.txt')
    $psi = New-Object System.Diagnostics.ProcessStartInfo 'cmd.exe'
    $psi.Arguments = '/c "' + ($commandTail -f $exe, $file) + '"'
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $p = [System.Diagnostics.Process]::Start($psi)
    $p.WaitForExit()
    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline) {
        if ((Test-Path -LiteralPath $file) -and (Get-Item -LiteralPath $file).Length -gt 3) { break }
        Start-Sleep -Milliseconds 200
    }
    $text = if (Test-Path -LiteralPath $file) { "$(Get-Content -LiteralPath $file -Raw)" } else { '' }
    if ($text -notmatch $pattern) { return "$label : output '$text' does not match $pattern" }
    Write-Host "ok   $label : $(($text -split "`n")[0].Trim())"
    return $null
}

$failures = @()
try {
    $failures += Test-NoConsole 'normal launch (cmd.exe /c exe)' $false
    if ($ShellExecute -or $env:CI) { $failures += Test-NoConsole 'normal launch (ShellExecute)' $true }
    $failures += Test-Redirect 'exe --help > f' '"{0}" --help > "{1}"' 'Usage:'
    $failures += Test-Redirect 'exe --version > f' '"{0}" --version > "{1}"' '^claude-history-gui [0-9]'
    $failures += Test-Redirect 'exe --help > f 2>&1' '"{0}" --help > "{1}" 2>&1' 'Usage:'
    $failures += Test-Redirect 'exe --version | more > f' '"{0}" --version | more > "{1}"' '^claude-history-gui [0-9]'
} finally {
    foreach ($n in $saved.Keys) { [Environment]::SetEnvironmentVariable($n, $saved[$n], 'Process') }
}

$failures = @($failures | Where-Object { $_ })
if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Output "FAIL $_" }
    exit 1
}
Write-Output 'console checks passed'
