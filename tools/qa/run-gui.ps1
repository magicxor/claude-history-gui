<#
.SYNOPSIS
  Runs claude-history-gui against a QA sandbox (DESIGN section 15.4), never against real data.

.DESCRIPTION
  Starts -Exe as a child process whose environment is the sandbox:
    USERPROFILE, HOME            = <Sandbox>
    CLAUDE_CONFIG_DIR            = <Sandbox>\.claude
    PI_CODING_AGENT_DIR          = <Sandbox>\.pi\agent
    XDG_DATA_HOME / XDG_CONFIG_HOME / XDG_CACHE_HOME / XDG_STATE_HOME = <Sandbox>\.local\share / .config / .cache / .local\state
    PI_CODING_AGENT_SESSION_DIR, PI_CONFIG_DIR, OMP_PROFILE, PI_PROFILE, TERMINAL,
    CHG_ALLOW_REAL_HOME, CHG_ALLOW_REAL_LAUNCH = removed
  The variables are set for the child only; this shell's environment is not changed.
  With -Scenario the (qa-feature) exe runs that scenario and the report is printed; with -Shots
  tools\qa\qa-shot-watch.ps1 captures the screenshots the scenario asks for (no input is injected).
  Only processes this script started are ever stopped.

.EXAMPLE
  .\tools\qa\run-gui.ps1 -Sandbox $env:TEMP\chg-sbx-1 -Exe target\release\claude-history-gui.exe `
      -Scenario crates\gui\qa\scenarios\smoke.qa -Out $env:TEMP\qa-out -Shots
#>
param(
    [Parameter(Mandatory = $true)][string]$Sandbox,
    [Parameter(Mandatory = $true)][string]$Exe,
    [string]$Scenario,
    [string]$Out,
    [string]$ModelDir,
    [switch]$Shots,
    # Extra arguments for the exe, as one string.
    [string]$ArgsLine = '',
    # Write the child's stdout to this file (for the --qa-real-console picker scenario).
    [string]$StdoutFile,
    [int]$TimeoutSec = 300
)
$ErrorActionPreference = 'Stop'

$sandbox = [System.IO.Path]::GetFullPath($Sandbox)
$exe = [System.IO.Path]::GetFullPath($Exe)
if (-not (Test-Path -LiteralPath (Join-Path $sandbox '.qa-sandbox'))) { throw "'$sandbox' has no .qa-sandbox marker; create it with tools\qa\new-sandbox.ps1." }
$profileDir = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
if ($profileDir -and [string]::Equals($sandbox.TrimEnd('\', '/'), $profileDir.TrimEnd('\', '/'), [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'Refusing: the sandbox is the real profile directory.'
}
if (-not (Test-Path -LiteralPath $exe)) { throw "Exe '$exe' not found." }

$argList = New-Object System.Collections.Generic.List[string]
if ($Scenario) {
    if (-not $Out) { throw '-Out is required with -Scenario.' }
    $scenarioPath = [System.IO.Path]::GetFullPath($Scenario)
    $outDir = [System.IO.Path]::GetFullPath($Out)
    New-Item -ItemType Directory -Force -Path $outDir | Out-Null
    $argList.Add('--qa-scenario'); $argList.Add("`"$scenarioPath`"")
    $argList.Add('--qa-out'); $argList.Add("`"$outDir`"")
    if ($ModelDir) { $argList.Add('--qa-model-dir'); $argList.Add("`"$([System.IO.Path]::GetFullPath($ModelDir))`"") }
}
if ($ArgsLine) { $argList.Add($ArgsLine) }

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $exe
$psi.Arguments = ($argList -join ' ')
$psi.UseShellExecute = $false
$psi.WorkingDirectory = $sandbox
$env_ = $psi.Environment
$sep = [System.IO.Path]::DirectorySeparatorChar
$set = @{
    'USERPROFILE'         = $sandbox
    'HOME'                = $sandbox
    'CLAUDE_CONFIG_DIR'   = (Join-Path $sandbox '.claude')
    'PI_CODING_AGENT_DIR' = (Join-Path $sandbox ".pi${sep}agent")
    'XDG_DATA_HOME'       = (Join-Path $sandbox ".local${sep}share")
    'XDG_CONFIG_HOME'     = (Join-Path $sandbox '.config')
    'XDG_CACHE_HOME'      = (Join-Path $sandbox '.cache')
    'XDG_STATE_HOME'      = (Join-Path $sandbox ".local${sep}state")
}
foreach ($name in $set.Keys) { $env_[$name] = $set[$name] }
foreach ($name in 'PI_CODING_AGENT_SESSION_DIR', 'PI_CONFIG_DIR', 'OMP_PROFILE', 'PI_PROFILE', 'TERMINAL', 'CHG_ALLOW_REAL_HOME', 'CHG_ALLOW_REAL_LAUNCH', 'CLAUDE_HISTORY_CLIPBOARD') {
    if ($env_.ContainsKey($name)) { $env_.Remove($name) | Out-Null }
}
if ($StdoutFile) { $psi.RedirectStandardOutput = $true }

$proc = [System.Diagnostics.Process]::Start($psi)
$watcher = $null
$stdoutTask = $null
if ($StdoutFile) { $stdoutTask = $proc.StandardOutput.ReadToEndAsync() }
if ($Shots) {
    if (-not $Scenario) { throw '-Shots needs -Scenario.' }
    $watchScript = Join-Path $PSScriptRoot 'qa-shot-watch.ps1'
    $shell = (Get-Process -Id $PID).Path
    $watcher = Start-Process -FilePath $shell -PassThru -WindowStyle Hidden -ArgumentList @('-NoProfile', '-File', "`"$watchScript`"", '-TargetPid', $proc.Id, '-Dir', "`"$outDir`"")
}

# Wait, sampling the working set the OS reports (DESIGN 15.8: <= 400 MB; the scenarios also check
# their own `process.*` probes at the points that matter).
$deadline = (Get-Date).AddSeconds($TimeoutSec)
$peakBytes = 0
$finished = $false
while ((Get-Date) -lt $deadline) {
    if ($proc.WaitForExit(500)) { $finished = $true; break }
    try {
        $proc.Refresh()
        $peakBytes = [Math]::Max($peakBytes, $proc.PeakWorkingSet64)
    } catch { }
}
if (-not $finished) {
    Write-Warning "Timed out after $TimeoutSec s; stopping the process this script started (pid $($proc.Id))."
    Stop-Process -Id $proc.Id -Force
    $proc.WaitForExit()
}
if ($peakBytes -gt 0) {
    Write-Output ("peak working set: {0:N1} MB" -f ($peakBytes / 1MB))
}
if ($stdoutTask) { Set-Content -LiteralPath $StdoutFile -Value $stdoutTask.Result -NoNewline -Encoding utf8 }
# The watcher may exit on its own between the check and the stop.
if ($watcher -and -not $watcher.HasExited) { Stop-Process -Id $watcher.Id -Force -ErrorAction SilentlyContinue }

if ($Scenario) {
    $report = Join-Path $outDir 'report.txt'
    if (Test-Path -LiteralPath $report) { Get-Content -LiteralPath $report } else { Write-Warning "No report at $report" }
}
Write-Output "exit code: $($proc.ExitCode)"
exit $proc.ExitCode
