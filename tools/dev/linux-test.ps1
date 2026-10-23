<#
.SYNOPSIS
  Runs cargo on Linux (Docker) for this checkout, with all build state in named volumes.

.DESCRIPTION
  Builds or reuses the local image claude-history-gui-linux-dev: rust:1-trixie with the Linux
  packages of .github/workflows/ci.yml and the toolchain of rust-toolchain.toml, running as the
  non-root user dev (uid 1000), so the permission tests of test_support::fault run as on CI instead
  of skipping as root. Then runs `cargo <arguments>` in a throw-away container (--rm) named
  claude-history-gui-cargo-<checkout>-<random>:

    /work                               this checkout (bind mount)
    /target (CARGO_TARGET_DIR)          volume claude-history-gui-target-main for the main checkout,
                                        claude-history-gui-target-<folder>-<hash> for a worktree
    cargo registry and git, the ONNX    shared volumes claude-history-gui-cargo-registry,
    Runtime download (ort-sys)          claude-history-gui-cargo-git, claude-history-gui-ort-cache

  Everything this script creates is named claude-history-gui-* and labelled claude-history-gui.dev
  (image, containers) or claude-history-gui.worktree=<checkout path> (target volumes). Nothing
  stays behind in containers or anonymous volumes, so the Docker disk grows only by these volumes.
  A worktree's new target volume is seeded from claude-history-gui-target-main (compiled
  dependencies only, timestamps kept): its first build compiles only the workspace crates. The
  paths are the same in every container, so the seed is reused as is. Build in the main checkout
  once to create the seed; a seed from an older main checkout is slower, never wrong.
  CARGO_BUILD_JOBS is passed through when set.

  Permanent, never deleted by this script: claude-history-gui-target-main (the seed of every
  worktree volume) and the shared caches claude-history-gui-cargo-registry, -cargo-git and
  -ort-cache. Losing them costs a cold build and the downloads again. The image is rebuilt
  automatically when it is missing.

  Options (before the cargo arguments):
    --update        pull the newest rust:1-trixie, rebuild the image and remove the old one
                    (once at the start of a session); alone, it runs no cargo command
    --drop-volume   delete this worktree's target volume (when the worktree is done); refused in
                    the main checkout
    --gc            delete the target volumes of worktrees that no longer exist (never the main
                    checkout's, even if the repo folder was moved)

  From a PowerShell prompt, quote the separator before clippy or test-binary arguments ('--'):
  PowerShell drops a bare -- when it calls a script; pwsh -File passes it on.

.EXAMPLE
  pwsh -NoProfile -File tools/dev/linux-test.ps1 test -p claude-history-core --locked
.EXAMPLE
  pwsh -NoProfile -File tools/dev/linux-test.ps1 clippy --workspace --all-targets --locked -- -D warnings
.EXAMPLE
  pwsh -NoProfile -File tools/dev/linux-test.ps1 --update
#>
$ErrorActionPreference = 'Stop'

$update = $false; $drop = $false; $gc = $false
$rest = @($args)
while ($rest.Count -gt 0 -and $rest[0] -in '--update', '--drop-volume', '--gc') {
    switch ($rest[0]) {
        '--update' { $update = $true }
        '--drop-volume' { $drop = $true }
        '--gc' { $gc = $true }
    }
    $rest = @($rest | Select-Object -Skip 1)
}
$cargoArgs = $rest
if (-not ($update -or $drop -or $gc -or $cargoArgs.Count)) { throw 'Nothing to do: pass cargo arguments or --update / --drop-volume / --gc (see Get-Help).' }

# Every Docker object of this script starts with this prefix, so it cannot be mistaken for the
# user's own images, containers and volumes.
$prefix = 'claude-history-gui'
$image = "$prefix-linux-dev"
$mainVolume = "$prefix-target-main"
$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))

function Invoke-Docker {
    docker @args
    if ($LASTEXITCODE -ne 0) { throw "docker $($args[0]) failed with exit code $LASTEXITCODE." }
}

function Test-Volume([string]$Name) {
    docker volume inspect $Name *> $null
    $LASTEXITCODE -eq 0
}

# This checkout's name in the volume and container names: main for the main checkout (first
# `git worktree list` entry), otherwise the folder name plus a hash of the full path.
$worktrees = @(git -C $root worktree list --porcelain)
if ($LASTEXITCODE -ne 0 -or -not ($worktrees[0] -match '^worktree (.+)$')) { throw "git worktree list failed in '$root'." }
$main = [System.IO.Path]::GetFullPath($Matches[1])
$isMain = [string]::Equals($root.TrimEnd('\'), $main.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)
if ($isMain) {
    $checkout = 'main'
} else {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $hash = -join ($sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($root.ToLowerInvariant())) | Select-Object -First 4 | ForEach-Object { $_.ToString('x2') })
    $leaf = ((Split-Path -Leaf $root).ToLowerInvariant() -replace '[^a-z0-9_.-]', '-')
    $checkout = "$leaf-$hash"
}
$volume = "$prefix-target-$checkout"

if ($gc) {
    foreach ($name in (docker volume ls -q --filter "label=$prefix.worktree")) {
        if ($name -eq $mainVolume) { continue }
        $path = docker volume inspect --format "{{ index .Labels `"$prefix.worktree`" }}" $name
        if ($path -and -not (Test-Path -LiteralPath $path)) {
            docker volume rm $name *> $null
            if ($LASTEXITCODE -eq 0) { Write-Host "Removed $name ($path no longer exists)." } else { Write-Warning "$name is in use; kept." }
        }
    }
}
if ($drop) {
    if ($isMain) { Write-Warning "$volume is the seed of every worktree volume and is kept; --drop-volume is for worktrees." }
    elseif (Test-Volume $volume) { Invoke-Docker volume rm $volume | Out-Null; Write-Host "Removed $volume." }
    else { Write-Host "$volume does not exist." }
}
if (-not ($update -or $cargoArgs.Count)) { exit 0 }

# The image. Rebuilt only when the base image (with --update) or the toolchain changes; otherwise
# every layer comes from the build cache.
$toolchainFile = Get-Content -Raw -LiteralPath (Join-Path $root 'rust-toolchain.toml')
if (-not ($toolchainFile -match '(?m)^channel\s*=\s*"([^"]+)"')) { throw 'rust-toolchain.toml has no channel.' }
$toolchain = $Matches[1]
$components = if ($toolchainFile -match '(?m)^components\s*=\s*\[([^\]]*)\]') { $Matches[1] -replace '[\s"]', '' } else { '' }
$dockerfile = @'
# Not bookworm: the static ONNX Runtime that ort-sys downloads needs glibc >= 2.38 and the
# libstdc++ of GCC >= 13 (undefined __isoc23_strtol and _M_replace_cold on Debian 12).
FROM rust:1-trixie
LABEL claude-history-gui.dev=1
# The Linux packages of .github/workflows/ci.yml (keep in sync).
RUN apt-get update \
 && apt-get install -y --no-install-recommends gcc g++ clang libfontconfig-dev libwayland-dev \
    libxkbcommon-x11-dev libx11-xcb-dev libssl-dev libzstd-dev libvulkan1 mesa-vulkan-drivers xvfb xterm \
 && rm -rf /var/lib/apt/lists/*
ARG TOOLCHAIN
ARG COMPONENTS
RUN rustup toolchain install "$TOOLCHAIN" --profile minimal ${COMPONENTS:+--component "$COMPONENTS"} \
 && rustup default "$TOOLCHAIN"
# Mount points owned by dev: a new named volume takes over the owner of its mount point.
RUN useradd -m -u 1000 dev \
 && mkdir -p /target /usr/local/cargo/registry /usr/local/cargo/git /home/dev/.cache/ort.pyke.io \
 && chown dev:dev /target /usr/local/cargo /usr/local/cargo/registry /usr/local/cargo/git \
 && chown -R dev:dev /home/dev/.cache
USER dev
'@
$build = @('build', '-t', $image, '--build-arg', "TOOLCHAIN=$toolchain", '--build-arg', "COMPONENTS=$components")
if ($update) {
    $dockerfile | docker @build --pull - | Out-Host
} else {
    $dockerfile | docker @build -q - | Out-Null
}
if ($LASTEXITCODE -ne 0) { throw "docker build failed with exit code $LASTEXITCODE." }
if ($update) { docker image prune -f --filter "label=$prefix.dev=1" | Out-Host }
if (-not $cargoArgs.Count) { exit 0 }

$runId = Get-Random -Maximum 1000000

# This checkout's target volume; a new worktree volume starts as a copy of the main one without
# the workspace crates, incremental/ and examples/.
if (-not (Test-Volume $volume)) {
    Invoke-Docker volume create --label "$prefix.worktree=$root" $volume | Out-Null
    if (-not $isMain -and (Test-Volume $mainVolume)) {
        Write-Host "Seeding $volume from $mainVolume..."
        $copy = "tar -C /from --exclude=./debug/incremental --exclude=./debug/examples " +
            "--exclude='*claude_history*' --exclude='*claude-history*' -cf - . | tar -C /target -xf -"
        Invoke-Docker run --rm --name "$prefix-seed-$checkout-$runId" --label "$prefix.dev=1" `
            --mount "type=volume,src=$mainVolume,dst=/from,readonly" `
            --mount "type=volume,src=$volume,dst=/target" $image sh -c $copy
    }
}

$run = @('run', '--rm', '--init', '--name', "$prefix-cargo-$checkout-$runId", '--label', "$prefix.dev=1",
    '-w', '/work',
    '--mount', "type=bind,src=$root,dst=/work",
    '--mount', "type=volume,src=$volume,dst=/target",
    '--mount', "type=volume,src=$prefix-cargo-registry,dst=/usr/local/cargo/registry",
    '--mount', "type=volume,src=$prefix-cargo-git,dst=/usr/local/cargo/git",
    '--mount', "type=volume,src=$prefix-ort-cache,dst=/home/dev/.cache/ort.pyke.io",
    '-e', 'CARGO_TARGET_DIR=/target', '-e', 'RUST_BACKTRACE=1')
if ($env:CARGO_BUILD_JOBS) { $run += @('-e', "CARGO_BUILD_JOBS=$env:CARGO_BUILD_JOBS") }
docker @run $image cargo @cargoArgs
exit $LASTEXITCODE
