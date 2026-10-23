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

# The main checkout of the repository that contains $Root: the first entry of `git worktree list`.
function Get-MainCheckout([string]$Root) {
    $worktrees = @(Invoke-Utf8 { git -C $Root worktree list --porcelain })
    if ($LASTEXITCODE -ne 0 -or -not ($worktrees[0] -match '^worktree (.+)$')) { throw "git worktree list failed in '$Root'." }
    [System.IO.Path]::GetFullPath($Matches[1])
}
