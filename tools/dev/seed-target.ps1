<#
.SYNOPSIS
  Seeds a fresh worktree's target\debug with the main checkout's compiled dependencies.

.DESCRIPTION
  Every git worktree has its own target directory, so the first build in a new worktree compiles
  the whole dependency tree (gpui, ONNX Runtime, ...): about 9 minutes on the dev machine. This
  script copies <main checkout>\target\debug into this worktree's target\debug with the file
  timestamps kept (cargo decides freshness by them) and leaves out what is rebuilt anyway: the
  workspace's own output (every package and target of `cargo metadata`), incremental\ and
  examples\. The next cargo command then compiles only the workspace crates.

  Cargo reuses only artifacts built with the same Cargo.lock, profile, features and toolchain and
  rebuilds the rest as usual, so a seed from an older main checkout is slower, never wrong. The
  main checkout is the first entry of `git worktree list`. Nothing is copied in the main checkout
  itself, when it has no target\debug, or when this worktree already has a target directory
  (delete it to reseed). A second run waits for the first one's copy (lock file in
  .git\claude-history-gui-dev), and a copy that was killed halfway is redone (target\.seeding
  exists only while it runs). Do not run it while a build is running in the main checkout, and do
  not start cargo in this worktree before it has finished.

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
$watch = [System.Diagnostics.Stopwatch]::StartNew()
$seeded = Invoke-Seed -Name "'$targetDir'" `
    -LockPath (Join-Path (Get-StateDir $root) "seed-target-$(Get-PathHash $root).lock") `
    -Marker (Join-Path $targetDir '.seeding') `
    -Exists { Test-Path -LiteralPath $targetDir } `
    -Remove { Remove-Item -LiteralPath $targetDir -Recurse -Force } `
    -Create {
        $patterns = @(Get-WorkspaceOutputPatterns $root)
        # /COPY:DAT and /DCOPY:DAT keep the timestamps. incremental\ and examples\ by full path, so
        # that a folder of the same name inside a build script's OUT_DIR is still copied.
        robocopy $source $dest /E /COPY:DAT /DCOPY:DAT /MT:16 /R:1 /W:1 /NFL /NDL /NJH /NP `
            /XD (Join-Path $source 'incremental') (Join-Path $source 'examples') @patterns `
            /XF '.cargo-lock' @patterns
        if ($LASTEXITCODE -ge 8) { throw "robocopy failed with exit code $LASTEXITCODE; run the script again." }
    }
if ($seeded) {
    Write-Host ("Seeded '{0}' from '{1}' in {2:N0} s. The next build compiles only the workspace crates." -f $dest, $source, $watch.Elapsed.TotalSeconds)
} else {
    Write-Host "'$targetDir' already exists. Nothing to do (delete it to reseed)."
}
exit 0
