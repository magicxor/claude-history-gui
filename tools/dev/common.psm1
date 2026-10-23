#Requires -Version 7.3
# Shared by the tools/dev scripts (Import-Module). PowerShell 7.3 or later: Windows PowerShell 5.1
# turns docker's stderr into terminating errors under ErrorActionPreference Stop, and before 7.3
# double quotes inside native arguments (docker --format templates) are dropped.
#
# A module, not a dot-sourced script: a script block that a script passes in (Invoke-Seed's
# callbacks, Invoke-Locked's and Invoke-CargoLocked's bodies) runs in the script's own variables,
# so the helpers' parameters and locals cannot shadow the script's (a $dirs in a callback once
# resolved to Invoke-CargoLocked's -Dirs). The scripts set the same preferences for themselves.
$ErrorActionPreference = 'Stop'
# Native-command behavior the helpers rely on, whatever the calling session set: a non-zero exit
# code is checked explicitly, and arguments are passed as on a default Windows pwsh.
$PSNativeCommandUseErrorActionPreference = $false
$PSNativeCommandArgumentPassing = 'Windows'

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

# The Win32 error code of an exception from a .NET call (32 sharing violation, 33 lock violation).
function Get-Win32Code([System.Exception]$Exception) {
    while ($Exception -is [System.Management.Automation.MethodInvocationException] -and $Exception.InnerException) {
        $Exception = $Exception.InnerException
    }
    $Exception.HResult -band 0xFFFF
}

# Runs $Body while holding an exclusive lock on the file $Path, so that concurrent runs for the same
# checkout wait for each other. A file, not a named mutex: an elevated and a normal process can both
# open it, and Windows releases the lock when its process dies. Only "in use" means busy; any other
# error stops the run. The wait is a loop of short sleeps, so Ctrl+C stops a waiting run. The lock
# cannot tell that a holder was killed halfway: callers mark half-done work themselves
# (Invoke-Seed).
function Invoke-Locked([string]$Path, [scriptblock]$Body) {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Path) | Out-Null
    $lock = $null
    $announced = $false
    while (-not $lock) {
        try {
            $lock = [System.IO.File]::Open($Path, 'OpenOrCreate', 'ReadWrite', 'None')
        } catch {
            if ((Get-Win32Code $_.Exception) -ne 32) { throw }
            if (-not $announced) {
                Write-Host "Waiting for another run to finish preparing ($(Split-Path -Leaf $Path))..."
                $announced = $true
            }
            Start-Sleep -Milliseconds 500
        }
    }
    try { & $Body } finally { $lock.Dispose() }
}

# Runs $Body while holding cargo's own locks (.cargo-build-lock and .cargo-artifact-lock, cargo 1.99)
# of each target profile directory in $Dirs, so that no cargo build writes there meanwhile: cargo
# waits with "Blocking waiting for file lock on build directory". Byte-range locks like cargo's own.
# The locks are taken without waiting and all dropped again when one is busy, so this never holds
# one lock while waiting for another that a cargo process holds.
function Invoke-CargoLocked([string[]]$Dirs, [scriptblock]$Body) {
    $paths = foreach ($dir in $Dirs) { foreach ($name in '.cargo-build-lock', '.cargo-artifact-lock') { Join-Path $dir $name } }
    $held = [System.Collections.Generic.List[System.IO.FileStream]]::new()
    $release = {
        foreach ($stream in $held) { $stream.Unlock(0, [long]::MaxValue); $stream.Dispose() }
        $held.Clear()
    }
    $announced = $false
    try {
        while ($true) {
            $busy = $false
            foreach ($path in $paths) {
                New-Item -ItemType Directory -Force -Path (Split-Path -Parent $path) | Out-Null
                $stream = [System.IO.File]::Open($path, 'OpenOrCreate', 'ReadWrite', 'ReadWrite, Delete')
                try {
                    $stream.Lock(0, [long]::MaxValue)
                    $held.Add($stream)
                } catch {
                    $stream.Dispose()
                    if ((Get-Win32Code $_.Exception) -ne 33) { throw }
                    $busy = $true
                    break
                }
            }
            if (-not $busy) { break }
            & $release
            if (-not $announced) {
                Write-Host "Waiting for cargo to finish building in $($Dirs -join ' and ')..."
                $announced = $true
            }
            Start-Sleep -Milliseconds 500
        }
        & $Body
    } finally {
        & $release
    }
}

# Prepares a build cache that starts as a copy (a worktree's target\ or a Docker volume), safe
# against concurrent runs and against a run killed halfway. Under the lock $LockPath:
#   * a marker left at $Marker means the last copy was killed halfway: & $Remove clears what it
#     left (the half-filled cache, or whatever the caller needs to undo), and the marker goes;
#   * if the cache is not there yet (& $Exists), the marker is written first (a run killed right
#     after creating the cache must not leave one that looks ready), & $Create creates and fills
#     it, and the marker is removed; if $Create fails, & $Remove runs and the marker goes too.
# Returns $true when it created the cache, $false when the cache was there already.
function Invoke-Seed {
    param([string]$Name, [string]$LockPath, [string]$Marker,
        [scriptblock]$Exists, [scriptblock]$Remove, [scriptblock]$Create)
    Invoke-Locked $LockPath {
        if (Test-Path -LiteralPath $Marker) {
            Write-Host "The last seeding of $Name was interrupted; seeding it again."
            & $Remove | Out-Host
            Remove-Item -LiteralPath $Marker -ErrorAction SilentlyContinue
        }
        if (& $Exists) { return $false }
        New-Item -ItemType File -Force -Path $Marker | Out-Null
        try {
            & $Create | Out-Host
        } catch {
            $failure = $_
            # When the cleanup fails too, the marker stays and the next run tries again.
            try {
                & $Remove | Out-Host
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

# The names of this repository's own build output, with '-' and '_': the workspace members and
# their targets (`cargo metadata --no-deps`), and every other path package (a Cargo.lock entry
# without a source: an in-tree crate pulled in by [patch] or a path dependency). A seed must leave
# them out, and not only to save space: a copied artifact of a path package keeps its name, and
# cargo would reuse it as fresh although this checkout's sources differ. Build scripts are not
# included (every dependency's compiled build script is named build_script_build too), nor a name
# that equals one of cargo's own directories. (Full metadata would need the manifests of every
# platform's dependencies, which are not downloaded.)
function Get-WorkspaceOutputNames([string]$Root) {
    $json = Invoke-Utf8 { cargo metadata --manifest-path (Join-Path $Root 'Cargo.toml') --format-version 1 --no-deps --offline 2>$null }
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed in '$Root'." }
    $names = foreach ($package in (($json -join "`n") | ConvertFrom-Json).packages) {
        $package.name
        foreach ($target in $package.targets) {
            if ($target.kind -notcontains 'custom-build') { $target.name }
        }
    }
    $lock = Get-Content -Raw -LiteralPath (Join-Path $Root 'Cargo.lock')
    $names = @($names) + @(foreach ($entry in ($lock -split '\[\[package\]\]')) {
            if ($entry -notmatch '(?m)^source = ' -and $entry -match '(?m)^name = "([^"]+)"') { $Matches[1] }
        })
    $names | ForEach-Object { $_; $_ -replace '-', '_' } | Sort-Object -Unique |
        Where-Object { $_ -notin 'build', 'deps', '.fingerprint', 'incremental', 'examples', 'native', 'tmp' }
}
