# Adds the viewer QA fixtures (DESIGN §15.3, §15.8; WP2 scenarios) to an
# existing QA sandbox made by tools/qa/new-sandbox.ps1. Writes only inside
# <Sandbox>\viewer\ and refuses anything that is not a sandbox.
#
#   ./prepare-sandbox.ps1 -Sandbox $env:TEMP\chg-sbx-1            # tour + error files + perf transcripts
#   ./prepare-sandbox.ps1 -Sandbox $env:TEMP\chg-sbx-1 -NoPerf    # without the big perf transcripts
#
# Files (all under <Sandbox>\viewer\):
#   markdown-tour.jsonl   markdown rules, code, tables (also a wide one), tool calls/results,
#                         a subagent, thinking, CJK/emoji (viewer-basics.qa, viewer-search-nav.qa)
#   empty.jsonl           a summary line only → "<file> contains no conversation messages"
#   folder.jsonl\         a directory → "Not a file: <p>" (single-file.qa)
#   perf-10k.jsonl        10,000 messages, 20 tool outputs of 5,000 lines (perf-viewer.qa)
#   perf-3k.jsonl         3,000 messages, 6 tool outputs of 5,000 lines (perf-viewer-3k run)
param(
    [Parameter(Mandatory = $true)][string]$Sandbox,
    [switch]$NoPerf
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

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$dest = Join-Path $sandbox 'viewer'
New-Item -ItemType Directory -Force -Path $dest | Out-Null

Copy-Item -LiteralPath (Join-Path $here 'markdown-tour.jsonl') -Destination (Join-Path $dest 'markdown-tour.jsonl') -Force
Set-Content -LiteralPath (Join-Path $dest 'empty.jsonl') -Value '{"type":"summary","summary":"Nothing here","leafUuid":"x"}' -Encoding utf8
New-Item -ItemType Directory -Force -Path (Join-Path $dest 'folder.jsonl') | Out-Null

if (-not $NoPerf) {
    $make = Join-Path $here 'make-perf-transcript.ps1'
    & $make -Out (Join-Path $dest 'perf-10k.jsonl') -Messages 10000 -BigOutputs 20 -LinesPerOutput 5000
    & $make -Out (Join-Path $dest 'perf-3k.jsonl') -Messages 3000 -BigOutputs 6 -LinesPerOutput 5000
}
Write-Output $dest
