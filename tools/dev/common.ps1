# Shared by the tools/dev scripts (dot-sourced).

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
# checkout wait for each other. The mutex alone cannot tell that a holder was killed halfway (with
# nobody waiting, a dead holder's mutex simply disappears): callers mark half-done work themselves.
function Invoke-Locked([string]$Name, [scriptblock]$Body) {
    $mutex = [System.Threading.Mutex]::new($false, "Global\$Name")
    try {
        try {
            if (-not $mutex.WaitOne(0)) {
                Write-Host "Waiting for another run to finish preparing ($Name)..."
                [void]$mutex.WaitOne()
            }
        } catch [System.Threading.AbandonedMutexException] { }
        & $Body
    } finally {
        $mutex.ReleaseMutex()
        $mutex.Dispose()
    }
}

# A short stable hash of a checkout path, for names that must differ per checkout.
function Get-PathHash([string]$Path) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    -join ($sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($Path.ToLowerInvariant())) | Select-Object -First 4 | ForEach-Object { $_.ToString('x2') })
}

# The main checkout of the repository that contains $Root: the first entry of `git worktree list`.
function Get-MainCheckout([string]$Root) {
    $worktrees = @(Invoke-Utf8 { git -C $Root worktree list --porcelain })
    if ($LASTEXITCODE -ne 0 -or -not ($worktrees[0] -match '^worktree (.+)$')) { throw "git worktree list failed in '$Root'." }
    [System.IO.Path]::GetFullPath($Matches[1])
}
