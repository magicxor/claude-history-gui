#Requires -Version 7.3
# Shared by the tools/dev scripts (dot-sourced). PowerShell 7.3 or later: Windows PowerShell 5.1
# turns docker's stderr into terminating errors under ErrorActionPreference Stop, and before 7.3
# double quotes inside native arguments (docker --format templates) are dropped.

# git and docker print UTF-8. Decode their output as such: with the console code page a non-ASCII
# path comes back garbled (the main checkout is not recognized, --gc takes a live worktree for a
# deleted one).
function Invoke-Utf8([scriptblock]$Command) {
    $previous = [Console]::OutputEncoding
    try {
        [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
        & $Command
    } finally {
        [Console]::OutputEncoding = $previous
    }
}

# Runs $Body while holding the machine-wide named mutex $Name, so that concurrent runs for the same
# checkout wait for each other. The wait is a loop of short waits, because one blocking wait cannot
# be interrupted with Ctrl+C. The mutex alone cannot tell that a holder was killed halfway (with
# nobody waiting, a dead holder's mutex simply disappears): callers mark half-done work themselves.
function Invoke-Locked([string]$Name, [scriptblock]$Body) {
    $mutex = [System.Threading.Mutex]::new($false, "Global\$Name")
    $acquired = $false
    try {
        try {
            $acquired = $mutex.WaitOne(0)
            if (-not $acquired) {
                Write-Host "Waiting for another run to finish preparing ($Name)..."
                while (-not $acquired) { $acquired = $mutex.WaitOne(500) }
            }
        } catch [System.Threading.AbandonedMutexException] {
            $acquired = $true
        }
        & $Body
    } finally {
        if ($acquired) { $mutex.ReleaseMutex() }
        $mutex.Dispose()
    }
}

# A short stable hash (8 hex digits) of $Text, for names that must differ with it.
function Get-ShortHash([string]$Text) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    -join ($sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($Text)) | Select-Object -First 4 | ForEach-Object { $_.ToString('x2') })
}

# The same for a checkout path (Windows paths are case-insensitive).
function Get-PathHash([string]$Path) { Get-ShortHash $Path.ToLowerInvariant() }

function Test-SamePath([string]$A, [string]$B) {
    [string]::Equals($A.TrimEnd('\'), $B.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)
}

# The checkout these scripts belong to (tools/dev/../..).
function Get-CheckoutRoot { [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..')) }

# The main checkout of the repository that contains $Root: the first entry of `git worktree list`.
function Get-MainCheckout([string]$Root) {
    $worktrees = @(Invoke-Utf8 { git -C $Root worktree list --porcelain })
    if ($LASTEXITCODE -ne 0 -or -not ($worktrees[0] -match '^worktree (.+)$')) { throw "git worktree list failed in '$Root'." }
    [System.IO.Path]::GetFullPath($Matches[1])
}

# The repository's shared git directory (the main checkout's .git), for state that belongs to this
# clone and goes away with it.
function Get-GitCommonDir([string]$Root) {
    $dir = Invoke-Utf8 { git -C $Root rev-parse --path-format=absolute --git-common-dir }
    if ($LASTEXITCODE -ne 0 -or -not $dir) { throw "git rev-parse failed in '$Root'." }
    [System.IO.Path]::GetFullPath($dir)
}
