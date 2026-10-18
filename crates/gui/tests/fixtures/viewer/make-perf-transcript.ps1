# Writes the synthetic viewer performance transcript of DESIGN §15.8 into a
# QA sandbox (never into a real profile). Same shape as
# `test_support::synth::transcript`: `Messages` alternating user/assistant
# messages, `BigOutputs` Bash results of `LinesPerOutput` lines each, every
# other assistant answer with a small rust code block.
#
#   ./make-perf-transcript.ps1 -Out <sandbox>\viewer\perf-10k.jsonl                      # 10k, 20 × 5000 lines
#   ./make-perf-transcript.ps1 -Out <sandbox>\viewer\perf-3k.jsonl -Messages 3000 -BigOutputs 6
param(
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$Messages = 10000,
    [int]$BigOutputs = 20,
    [int]$LinesPerOutput = 5000
)
$ErrorActionPreference = 'Stop'
# Only ever write inside a QA sandbox: some ancestor of -Out must carry the
# .qa-sandbox marker, and the real ~/.claude is never a target.
$full = [System.IO.Path]::GetFullPath($Out)
$probe = Split-Path -Parent $full
$marked = $false
while ($probe) {
    if (Test-Path -LiteralPath (Join-Path $probe '.qa-sandbox')) { $marked = $true; break }
    $probe = Split-Path -Parent $probe
}
if (-not $marked) { throw "Refusing: '$full' is not inside a QA sandbox (no .qa-sandbox marker in any parent)." }
$profileDir = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
if ($profileDir -and $full.StartsWith((Join-Path $profileDir '.claude'), [System.StringComparison]::OrdinalIgnoreCase) -and -not (Test-Path -LiteralPath (Join-Path $profileDir '.qa-sandbox'))) {
    throw 'Refusing: the target is inside the real profile.'
}
$dir = Split-Path -Parent $Out
if ($dir) { New-Item -ItemType Directory -Force $dir | Out-Null }
$uuid = [IO.Path]::GetFileNameWithoutExtension($Out)
$every = if ($BigOutputs -gt 0) { [Math]::Max(2, [Math]::Floor($Messages / $BigOutputs)) } else { [int]::MaxValue }
if ($every % 2 -eq 1) { $every += 1 }
$outputs = 0
$writer = New-Object System.IO.StreamWriter($Out, $false, (New-Object System.Text.UTF8Encoding($false)))
$writer.NewLine = "`n"
try {
    for ($ix = 0; $ix -lt $Messages; $ix++) {
        $stamp = '2026-09-01T{0:D2}:{1:D2}:{2:D2}Z' -f ([int]([Math]::Floor($ix / 3600)) % 24), ([int]([Math]::Floor($ix / 60)) % 60), ($ix % 60)
        if ($ix % 2 -eq 0) {
            $writer.WriteLine('{"type":"user","sessionId":"' + $uuid + '","timestamp":"' + $stamp + '","message":{"role":"user","content":"Question ' + $ix + ': how does part ' + $ix + ' work?"}}')
        } elseif ($outputs -lt $BigOutputs -and ($ix % $every) -eq 1) {
            $outputs++
            $tool = "toolu_$ix"
            $writer.WriteLine('{"type":"assistant","sessionId":"' + $uuid + '","timestamp":"' + $stamp + '","message":{"role":"assistant","content":[{"type":"text","text":"Running a command."},{"type":"tool_use","id":"' + $tool + '","name":"Bash","input":{"command":"seq ' + $LinesPerOutput + '"}}]}}')
            $body = New-Object System.Text.StringBuilder
            for ($n = 1; $n -le $LinesPerOutput; $n++) { [void]$body.Append("output line $n\n") }
            $writer.WriteLine('{"type":"user","sessionId":"' + $uuid + '","timestamp":"' + $stamp + '","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"' + $tool + '","content":"' + $body.ToString() + '"}]}}')
        } else {
            $writer.WriteLine('{"type":"assistant","sessionId":"' + $uuid + '","timestamp":"' + $stamp + '","message":{"role":"assistant","content":[{"type":"text","text":"Answer ' + $ix + ': part ' + $ix + ' works like this.\n\n```rust\nfn part_' + $ix + '() {}\n```"}]}}')
        }
    }
} finally {
    $writer.Close()
}
Write-Output "wrote $Out"
