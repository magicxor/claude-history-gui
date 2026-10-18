<#
.SYNOPSIS
  Creates a QA sandbox home from a corpus template (DESIGN section 15.4).

.DESCRIPTION
  Copies -Source (a template home such as the repo's crates/gui/tests/fixtures/qa-home, or any
  directory holding .claude/projects, .pi/..., never the real profile) into the new directory
  -Dest and prepares it for claude-history-gui:
    * <Dest>\.qa-sandbox       the marker the sandbox gate requires (debug and qa builds)
    * <Dest>\.config\claude-history\, <Dest>\.local\state\
    * <Dest>\bin\fake-agent.*  a harmless recorder script (appends its arguments to <Dest>\launches.log)
    * <Dest>\.config\claude-history\gui.toml  with [gui] claude_path / pi_path / omp_path pointing at it,
      so a Resume in the sandbox can never start a real claude, pi or omp.
  Never use the template in place and never point -Dest at a real profile: the script refuses
  the current user's profile directory, anything inside it that already exists, and an existing
  -Dest that is not a previous sandbox.

  -WithWorkspace additionally creates <Dest>\work\resume-demo and a Claude conversation whose cwd is
  that folder (the newest conversation, so it is the selected first row). The corpus cwds
  (C:\sbx\api, ...) do not exist on a CI runner, where core answers "Choose a workspace folder to
  resume this session"; scenarios that must reach a recorded terminal launch (settings-keys.qa)
  need this conversation. Nothing else in the corpus changes, so row counts of other scenarios are
  unaffected unless they ask for it.

.EXAMPLE
  .\tools\qa\new-sandbox.ps1 -Source crates\gui\tests\fixtures\qa-home -Dest $env:TEMP\chg-sbx-1
#>
param(
    [Parameter(Mandatory = $true)][string]$Source,
    [Parameter(Mandatory = $true)][string]$Dest,
    # Replace an existing sandbox (a directory that holds the .qa-sandbox marker).
    [switch]$Force,
    # Add a conversation whose project folder exists inside the sandbox (see .DESCRIPTION).
    [switch]$WithWorkspace
)
$ErrorActionPreference = 'Stop'

function Resolve-Full([string]$path) { return [System.IO.Path]::GetFullPath($path) }
function Same-Path([string]$a, [string]$b) {
    return [string]::Equals((Resolve-Full $a).TrimEnd('\', '/'), (Resolve-Full $b).TrimEnd('\', '/'), [System.StringComparison]::OrdinalIgnoreCase)
}

$source = Resolve-Full $Source
$dest = Resolve-Full $Dest
if (-not (Test-Path -LiteralPath $source -PathType Container)) { throw "Source '$source' is not a directory." }

# The OS profile directory (from the OS, not from variables the caller may have changed).
$profileDir = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
if ($profileDir) {
    if (Same-Path $dest $profileDir) { throw "Refusing: -Dest is the real profile directory." }
    if (Same-Path $source $profileDir) { throw "Refusing: -Source is the real profile directory; use a corpus template." }
    $realClaude = Join-Path $profileDir '.claude'
    if ($dest.StartsWith((Resolve-Full $realClaude), [System.StringComparison]::OrdinalIgnoreCase)) { throw "Refusing: -Dest is inside the real .claude directory." }
}
if (Same-Path $source $dest) { throw "-Source and -Dest are the same directory." }

if (Test-Path -LiteralPath $dest) {
    $marker = Join-Path $dest '.qa-sandbox'
    if ($Force -and (Test-Path -LiteralPath $marker)) {
        Remove-Item -LiteralPath $dest -Recurse -Force
    } elseif ((Get-ChildItem -LiteralPath $dest -Force | Measure-Object).Count -eq 0) {
        # An empty directory is fine.
    } else {
        throw "Refusing: '$dest' exists and is not empty (pass -Force only for an existing sandbox)."
    }
}

New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item -Path (Join-Path $source '*') -Destination $dest -Recurse -Force
# Hidden entries (.claude, .pi, ...) are not matched by '*' on every platform.
foreach ($entry in Get-ChildItem -LiteralPath $source -Force | Where-Object { $_.Name.StartsWith('.') }) {
    $target = Join-Path $dest $entry.Name
    if (-not (Test-Path -LiteralPath $target)) { Copy-Item -LiteralPath $entry.FullName -Destination $target -Recurse -Force }
}

New-Item -ItemType File -Force -Path (Join-Path $dest '.qa-sandbox') | Out-Null
$configDir = Join-Path $dest '.config\claude-history'
New-Item -ItemType Directory -Force -Path $configDir | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $dest '.local\state') | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $dest '.cache') | Out-Null
$bin = Join-Path $dest 'bin'
New-Item -ItemType Directory -Force -Path $bin | Out-Null

$log = Join-Path $dest 'launches.log'
if ($IsLinux -or $IsMacOS) {
    $agent = Join-Path $bin 'fake-agent.sh'
    Set-Content -LiteralPath $agent -Value "#!/bin/sh`necho `"`$0 `$*`" >> '$log'`nsleep 1`n" -NoNewline
    & chmod +x $agent
} else {
    $agent = Join-Path $bin 'fake-agent.cmd'
    Set-Content -LiteralPath $agent -Value "@echo off`r`necho %~f0 %* >> `"$log`"`r`n" -NoNewline
}

# TOML literal strings (single quotes) need no escaping.
$gui = @"
# Written by tools/qa/new-sandbox.ps1: every launcher is the harmless recorder.
[gui]
claude_path = '$agent'
pi_path = '$agent'
omp_path = '$agent'
"@
Set-Content -LiteralPath (Join-Path $configDir 'gui.toml') -Value $gui -Encoding utf8

if ($WithWorkspace) {
    $workspace = Join-Path $dest 'work/resume-demo'
    New-Item -ItemType Directory -Force -Path $workspace | Out-Null
    $workspace = (Resolve-Path -LiteralPath $workspace).Path
    # Claude Code's project folder name: every character that is not alphanumeric or '-' becomes '-'.
    $encoded = [regex]::Replace($workspace, '[^A-Za-z0-9-]', '-')
    $projectDir = Join-Path $dest ".claude/projects/$encoded"
    New-Item -ItemType Directory -Force -Path $projectDir | Out-Null
    $session = '00000000-0000-4000-8000-0000000000f1'
    $now = [DateTime]::UtcNow
    $lines = @(
        @{ type = 'summary'; summary = 'Resumable demo workspace'; leafUuid = "leaf-$session" },
        @{ type = 'user'; sessionId = $session; cwd = $workspace; timestamp = $now.AddMinutes(-2).ToString('yyyy-MM-ddTHH:mm:ssZ'); message = @{ role = 'user'; content = 'Resume me from the sandbox workspace.' } },
        @{ type = 'assistant'; sessionId = $session; cwd = $workspace; timestamp = $now.AddMinutes(-1).ToString('yyyy-MM-ddTHH:mm:ssZ'); message = @{ role = 'assistant'; model = 'claude-opus-4-5'; content = @(@{ type = 'text'; text = 'Done.' }); usage = @{ input_tokens = 10; output_tokens = 5 } } }
    ) | ForEach-Object { $_ | ConvertTo-Json -Compress -Depth 8 }
    $file = Join-Path $projectDir "$session.jsonl"
    Set-Content -LiteralPath $file -Value $lines -Encoding utf8
    (Get-Item -LiteralPath $file).LastWriteTimeUtc = $now
}

Write-Output $dest
