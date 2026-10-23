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
# Native commands: exit codes are checked explicitly (robocopy's success codes are 1-7), whatever
# the calling session set.
$PSNativeCommandUseErrorActionPreference = $false
$PSNativeCommandArgumentPassing = 'Windows'
Import-Module (Join-Path $PSScriptRoot 'common.psm1') -Force -DisableNameChecking

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
$seedMarker = Join-Path $targetDir '.seeding'
$watch = [System.Diagnostics.Stopwatch]::StartNew()
$seeded = Invoke-Seed -Name "'$dest'" `
    -LockPath (Join-Path (Get-StateDir $root) "seed-target-$(Get-PathHash $root).lock") `
    -Marker $seedMarker `
    -Exists { Test-Path -LiteralPath $done } `
    -Remove {
        # The robocopy of a killed run may still be copying: stop it, identified by the process id
        # and start time that the marker records (never a robocopy this script did not start). The
        # files it left need no cleanup: the next copy takes every file whose size or time differs,
        # a cut-off one too.
        $recorded = Get-Content -LiteralPath $seedMarker -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($recorded -match '^(\d+):(\d+)$') {
            $copier = Get-Process -Id ([int]$Matches[1]) -ErrorAction SilentlyContinue
            if ($copier -and $copier.StartTime.ToFileTimeUtc() -eq [long]$Matches[2]) {
                Stop-Process -Id $copier.Id -Force
                $copier.WaitForExit()
            }
        }
    } `
    -Create {
        $names = @(Get-WorkspaceOutputNames $root)
        $excludeFiles = foreach ($n in $names) { $n; "$n-*"; "$n.*"; "lib$n-*"; "lib$n.*" }
        $excludeDirs = foreach ($n in $names) { "$n-*" }
        # /COPY:DAT and /DCOPY:DAT keep the timestamps. incremental\ and examples\ by full path, so
        # that a folder of the same name inside a build script's OUT_DIR is still copied.
        $arguments = @($source, $dest, '/E', '/COPY:DAT', '/DCOPY:DAT', '/MT:16', '/R:1', '/W:1', '/NFL', '/NDL', '/NJH', '/NP',
            '/XD', (Join-Path $source 'incremental'), (Join-Path $source 'examples')) + $excludeDirs +
            @('/XF', '.cargo-*lock', '*.pdb', 'build-script-*.exe') + $excludeFiles
        Invoke-CargoLocked @($source, $dest) {
            # A process of its own, so that its id and start time can go into the marker.
            $start = [System.Diagnostics.ProcessStartInfo]::new('robocopy.exe')
            foreach ($argument in $arguments) { $start.ArgumentList.Add($argument) }
            $copier = [System.Diagnostics.Process]::Start($start)
            Set-Content -LiteralPath $seedMarker -Value "$($copier.Id):$($copier.StartTime.ToFileTimeUtc())"
            $copier.WaitForExit()
            if ($copier.ExitCode -ge 8) { throw "robocopy failed with exit code $($copier.ExitCode); run the script again." }
        }
        New-Item -ItemType File -Force -Path $done | Out-Null
    }
if ($seeded) {
    Write-Host ("Seeded '{0}' from '{1}' in {2:N0} s. The next build compiles only the workspace crates." -f $dest, $source, $watch.Elapsed.TotalSeconds)
} else {
    Write-Host "'$dest' was seeded already. Nothing to do (delete '$done' to seed again)."
}
exit 0
