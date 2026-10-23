<#
.SYNOPSIS
  Seeds a worktree's target\debug with the main checkout's compiled dependencies.

.DESCRIPTION
  Every git worktree has its own target directory, so the first build in a new worktree compiles
  the whole dependency tree (gpui, ONNX Runtime, ...): about 9 minutes on the dev machine. This
  script copies <main checkout>\target\debug into this worktree's target\debug with the file
  timestamps kept (cargo decides freshness by them) and leaves out what must or need not be
  copied: the repository's own build output (every path package and its targets), incremental\,
  examples\, PDBs and the duplicate build-script-build.exe names. The next cargo command then
  compiles only the workspace crates.

  The copy runs under cargo's own build locks of both directories: a build in the main checkout
  cannot change the source halfway, and a cargo in this worktree (rust-analyzer started by an
  editor or a language-server tool, for one) waits for the copy. A target\ that such a cargo has
  already created is filled in as well; target\.seeded marks a finished seed. A second run waits
  for the first one's copy (lock file in .git\claude-history-gui-dev), and a copy that was killed
  halfway is redone (target\.seeding exists only while it runs).

  Cargo reuses only artifacts built with the same Cargo.lock, profile, features and toolchain and
  rebuilds the rest as usual, so a seed from an older main checkout is slower, not wrong. The main
  checkout is the first entry of `git worktree list`; nothing is copied there, nor when it has no
  target\debug yet.

.EXAMPLE
  pwsh -NoProfile -File tools/dev/seed-target.ps1
#>
#Requires -Version 7.3
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'common.ps1')

$root = Get-CheckoutRoot
$main = Get-MainCheckout $root

if ($env:CARGO_TARGET_DIR) {
    Write-Host "CARGO_TARGET_DIR is set ($env:CARGO_TARGET_DIR); this worktree does not use target\. Nothing to do."
    exit 0
}
if (Test-SamePath $root $main) {
    Write-Host "'$root' is the main checkout. Nothing to do."
    exit 0
}
$source = Join-Path $main 'target\debug'
if (-not (Test-Path -LiteralPath $source)) {
    Write-Host "The main checkout has no '$source' yet; the first build here compiles everything."
    exit 0
}

$targetDir = Join-Path $root 'target'
$dest = Join-Path $targetDir 'debug'
$done = Join-Path $targetDir '.seeded'
$watch = [System.Diagnostics.Stopwatch]::StartNew()
$seeded = Invoke-Seed -Name "'$dest'" `
    -LockPath (Join-Path (Get-StateDir $root) "seed-target-$(Get-PathHash $root).lock") `
    -Marker (Join-Path $targetDir '.seeding') `
    -Exists { Test-Path -LiteralPath $done } `
    -Remove {
        # The robocopy of a killed run may still be copying: stop it. The files it left need no
        # cleanup: the next copy takes every file whose size or time differs, a cut-off one too.
        Get-CimInstance Win32_Process -Filter "Name = 'Robocopy.exe'" |
            Where-Object { $_.CommandLine -and $_.CommandLine.Contains($dest) } |
            ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    } `
    -Create {
        $names = @(Get-WorkspaceOutputNames $root)
        $files = foreach ($n in $names) { $n; "$n-*"; "$n.*"; "lib$n-*"; "lib$n.*" }
        $dirs = foreach ($n in $names) { "$n-*" }
        Invoke-CargoLocked @($source, $dest) {
            # /COPY:DAT and /DCOPY:DAT keep the timestamps. incremental\ and examples\ by full path,
            # so that a folder of the same name inside a build script's OUT_DIR is still copied.
            robocopy $source $dest /E /COPY:DAT /DCOPY:DAT /MT:16 /R:1 /W:1 /NFL /NDL /NJH /NP `
                /XD (Join-Path $source 'incremental') (Join-Path $source 'examples') @dirs `
                /XF '.cargo-*lock' '*.pdb' 'build-script-*.exe' @files
            if ($LASTEXITCODE -ge 8) { throw "robocopy failed with exit code $LASTEXITCODE; run the script again." }
        }
        New-Item -ItemType File -Force -Path $done | Out-Null
    }
if ($seeded) {
    Write-Host ("Seeded '{0}' from '{1}' in {2:N0} s. The next build compiles only the workspace crates." -f $dest, $source, $watch.Elapsed.TotalSeconds)
} else {
    Write-Host "'$dest' was seeded already. Nothing to do (delete '$done' to seed again)."
}
exit 0
