<#
.SYNOPSIS
  Adds a viewer perf transcript to a QA sandbox as an ordinary list row (perf-viewer-main.qa, V-094).

.DESCRIPTION
  Copies -Source (e.g. <Sandbox>\viewer\perf-3k.jsonl, made by crates\gui\tests\fixtures\viewer\prepare-sandbox.ps1)
  to <Sandbox>\.claude\projects\<project>\ffffffff-1c2d-4e5f-8a9b-0c1d2e3f4a5b.jsonl, so that the main window
  lists it among the synthetic corpus and a UUID query finds it. Writes only inside the sandbox; refuses a
  directory without the .qa-sandbox marker and the real profile directory.

.EXAMPLE
  .\tools\qa\add-perf-session.ps1 -Sandbox $env:TEMP\chg-sbx-perf -Source $env:TEMP\chg-sbx-perf\viewer\perf-3k.jsonl
#>
param(
    [Parameter(Mandatory = $true)][string]$Sandbox,
    [Parameter(Mandatory = $true)][string]$Source
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
$source = [System.IO.Path]::GetFullPath($Source)
if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw "Source '$source' not found." }

$onWindows = -not ($IsLinux -or $IsMacOS)
$dirName = if ($onWindows) { 'C--synthetic-project-00' } else { '-synthetic-project-00' }
$dir = Join-Path (Join-Path (Join-Path $sandbox '.claude') 'projects') $dirName
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$dest = Join-Path $dir 'ffffffff-1c2d-4e5f-8a9b-0c1d2e3f4a5b.jsonl'
Copy-Item -LiteralPath $source -Destination $dest -Force
Write-Output $dest
