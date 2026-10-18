<#
.SYNOPSIS
  Adds synthetic Claude conversations to an existing QA sandbox (perf-list.qa, DESIGN section 15.8).

.DESCRIPTION
  Writes -Count conversations in the layout of the GUI tests' test_support::synth::corpus:
  10 projects (<base>\project-00 ... project-09, base C:\synthetic or /synthetic), session UUIDs
  "{1000+i:x8}-1c2d-4e5f-8a9b-0c1d2e3f4a5b", a summary "Synthetic conversation <i>" and one
  user/assistant exchange each. Files go only under <Sandbox>\.claude\projects; the script refuses
  a directory without the .qa-sandbox marker and the real profile directory.

.EXAMPLE
  .\tools\qa\new-sandbox.ps1 -Source <template home> -Dest $env:TEMP\chg-sbx-perf
  .\tools\qa\add-synth-corpus.ps1 -Sandbox $env:TEMP\chg-sbx-perf -Count 20000
#>
param(
    [Parameter(Mandatory = $true)][string]$Sandbox,
    [int]$Count = 20000
)
$ErrorActionPreference = 'Stop'

$sandbox = [System.IO.Path]::GetFullPath($Sandbox)
if (-not (Test-Path -LiteralPath (Join-Path $sandbox '.qa-sandbox'))) {
    throw "'$sandbox' has no .qa-sandbox marker; create it with tools\qa\new-sandbox.ps1 first."
}
$profileDir = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
if ($profileDir -and [string]::Equals($sandbox.TrimEnd('\', '/'), $profileDir.TrimEnd('\', '/'), [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'Refusing: the sandbox is the real profile directory.'
}

$onWindows = -not ($IsLinux -or $IsMacOS)
$projects = Join-Path (Join-Path $sandbox '.claude') 'projects'
$utf8 = New-Object System.Text.UTF8Encoding($false)
for ($ix = 0; $ix -lt $Count; $ix++) {
    $project = 'project-{0:D2}' -f ($ix % 10)
    if ($onWindows) {
        $cwdJson = 'C:\\synthetic\\' + $project
        $dirName = 'C--synthetic-' + $project
    } else {
        $cwdJson = '/synthetic/' + $project
        $dirName = '-synthetic-' + $project
    }
    $dir = Join-Path $projects $dirName
    if ($ix -lt 10) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    $uuid = '{0:x8}-1c2d-4e5f-8a9b-0c1d2e3f4a5b' -f (1000 + $ix)
    $title = "Synthetic conversation $ix"
    $day = '{0:D2}' -f (1 + ($ix % 28))
    $lines = @(
        "{`"type`":`"summary`",`"summary`":`"$title`",`"leafUuid`":`"leaf-$uuid`"}",
        "{`"type`":`"user`",`"sessionId`":`"$uuid`",`"cwd`":`"$cwdJson`",`"timestamp`":`"2026-09-${day}T10:00:00Z`",`"message`":{`"role`":`"user`",`"content`":`"${title}: question 1`"}}",
        "{`"type`":`"assistant`",`"sessionId`":`"$uuid`",`"cwd`":`"$cwdJson`",`"timestamp`":`"2026-09-${day}T10:01:00Z`",`"message`":{`"role`":`"assistant`",`"model`":`"claude-opus-4-5`",`"content`":[{`"type`":`"text`",`"text`":`"${title}: answer 1`"}]}}"
    )
    [System.IO.File]::WriteAllText((Join-Path $dir "$uuid.jsonl"), (($lines -join "`n") + "`n"), $utf8)
}
Write-Output "$Count conversations under $projects"
