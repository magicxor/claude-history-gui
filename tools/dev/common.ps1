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

# Runs $Body while holding an exclusive lock on the file $Path, so that concurrent runs for the same
# checkout wait for each other. A file, not a named mutex: an elevated and a normal process can both
# open it, and Windows releases the lock when its process dies. The wait is a loop of short sleeps,
# so Ctrl+C stops a waiting run. The lock cannot tell that a holder was killed halfway: callers mark
# half-done work themselves (Invoke-Seed).
function Invoke-Locked([string]$Path, [scriptblock]$Body) {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Path) | Out-Null
    $lock = $null
    $announced = $false
    while (-not $lock) {
        try {
            $lock = [System.IO.File]::Open($Path, 'OpenOrCreate', 'ReadWrite', 'None')
        } catch [System.IO.IOException] {
            if (-not $announced) {
                Write-Host "Waiting for another run to finish preparing ($(Split-Path -Leaf $Path))..."
                $announced = $true
            }
            Start-Sleep -Milliseconds 500
        }
    }
    try { & $Body } finally { $lock.Dispose() }
}

# Prepares a build cache that starts as a copy (a worktree's target\ or a Docker volume), safe
# against concurrent runs and against a run killed halfway. Under the lock $LockPath:
#   * a marker left at $Marker means the last copy was killed halfway: the half-filled cache is
#     removed (& $Remove) and the marker with it;
#   * if the cache is missing (& $Exists), the marker is written first (a run killed right after
#     creating the cache must not leave one that looks ready), & $Create creates and fills it, and
#     the marker is removed; if $Create fails, the half-filled cache and the marker go too.
# Returns $true when it created the cache, $false when the cache was there already.
function Invoke-Seed {
    param([string]$Name, [string]$LockPath, [string]$Marker,
        [scriptblock]$Exists, [scriptblock]$Remove, [scriptblock]$Create)
    Invoke-Locked $LockPath {
        if (Test-Path -LiteralPath $Marker) {
            if (& $Exists) {
                Write-Host "The last seeding of $Name was interrupted; seeding it again."
                & $Remove | Out-Host
            }
            Remove-Item -LiteralPath $Marker -ErrorAction SilentlyContinue
        }
        if (& $Exists) { return $false }
        New-Item -ItemType File -Force -Path $Marker | Out-Null
        try {
            & $Create | Out-Host
        } catch {
            $failure = $_
            # When the cache cannot be removed, the marker stays and the next run tries again.
            try {
                if (& $Exists) { & $Remove | Out-Host }
                Remove-Item -LiteralPath $Marker -ErrorAction SilentlyContinue
            } catch { }
            throw $failure
        }
        Remove-Item -LiteralPath $Marker -ErrorAction SilentlyContinue
        $true
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

# The checkout these scripts belong to, as git reports it: git resolves junctions, symbolic links
# and subst drives, so this compares correctly with Get-MainCheckout.
function Get-CheckoutRoot {
    $top = Invoke-Utf8 { git -C $PSScriptRoot rev-parse --show-toplevel }
    if ($LASTEXITCODE -ne 0 -or -not $top) { throw "git rev-parse failed in '$PSScriptRoot'." }
    [System.IO.Path]::GetFullPath($top)
}

# The checkouts of the repository that contains $Root, the main one first, from `git worktree list`
# (-z: NUL-terminated fields, whose paths git never quotes or escapes).
function Get-Worktrees([string]$Root) {
    $output = Invoke-Utf8 { git -C $Root worktree list --porcelain -z }
    if ($LASTEXITCODE -ne 0) { throw "git worktree list failed in '$Root'." }
    foreach ($field in ((@($output) -join "`n") -split "`0")) {
        if ($field -match '^worktree (.+)$') { [System.IO.Path]::GetFullPath($Matches[1]) }
    }
}

function Get-MainCheckout([string]$Root) {
    $main = @(Get-Worktrees $Root) | Select-Object -First 1
    if (-not $main) { throw "git worktree list failed in '$Root'." }
    $main
}

# The directory for these scripts' state (locks, seed markers) in the repository's shared git
# directory: it belongs to this clone and goes away with it.
function Get-StateDir([string]$Root) {
    $dir = Invoke-Utf8 { git -C $Root rev-parse --path-format=absolute --git-common-dir }
    if ($LASTEXITCODE -ne 0 -or -not $dir) { throw "git rev-parse failed in '$Root'." }
    Join-Path ([System.IO.Path]::GetFullPath($dir)) 'claude-history-gui-dev'
}

# File-name patterns of the workspace's own build output (its packages and their targets, with '-'
# and '_'), which a seed leaves out: cargo rebuilds them anyway. Build scripts are not included:
# every dependency's compiled build script is named build_script_build too.
function Get-WorkspaceOutputPatterns([string]$Root) {
    $json = Invoke-Utf8 { cargo metadata --manifest-path (Join-Path $Root 'Cargo.toml') --format-version 1 --no-deps --offline 2>$null }
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed in '$Root'." }
    $names = foreach ($package in (($json -join "`n") | ConvertFrom-Json).packages) {
        $package.name
        foreach ($target in $package.targets) {
            if ($target.kind -notcontains 'custom-build') { $target.name }
        }
    }
    $names = $names | ForEach-Object { $_; $_ -replace '-', '_' } | Sort-Object -Unique
    foreach ($n in $names) { $n; "$n-*"; "$n.*"; "lib$n-*"; "lib$n.*" }
}
