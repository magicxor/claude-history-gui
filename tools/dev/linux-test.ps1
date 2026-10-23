<#
.SYNOPSIS
  Runs cargo on Linux (Docker) for this checkout, with all build state in named volumes.

.DESCRIPTION
  Builds or reuses the local image claude-history-gui-linux-dev:<toolchain>-<hash>: rust:1-trixie
  with the Linux packages of .github/workflows/ci.yml and the toolchain of rust-toolchain.toml,
  running as the non-root user dev (uid 1000), so the permission tests of test_support::fault run
  as on CI instead of skipping as root. The tag differs per toolchain and image definition, so
  checkouts on different toolchains never run each other's image. Then runs `cargo <arguments>`
  in a throw-away container (--rm) named claude-history-gui-cargo-<checkout>-<random>:

    /work                               this checkout (bind mount)
    /target (CARGO_TARGET_DIR)          volume claude-history-gui-target-main for the main checkout,
                                        claude-history-gui-target-<folder>-<hash> for a worktree
    CARGO_HOME (registry, git, and      shared volume claude-history-gui-cargo-home: one volume, so
    the locks that guard them)          concurrent containers share cargo's cache locks
    the ONNX Runtime download (ort-sys) shared volume claude-history-gui-ort-cache

  Everything this script creates is named claude-history-gui-* and labelled claude-history-gui.dev
  (image, containers) or claude-history-gui.worktree=<checkout path> (target volumes). Nothing
  stays behind in containers or anonymous volumes, so the Docker disk grows only by these volumes.
  A worktree's new target volume is seeded from claude-history-gui-target-main (compiled
  dependencies only, timestamps kept): its first build compiles only the workspace crates. The
  paths are the same in every container, so the seed is reused as is. Concurrent runs for the same
  checkout wait until its volume is ready, and a copy that was killed halfway is redone. Build in
  the main checkout once to create the seed; a seed from an older main checkout is slower, never
  wrong.
  CARGO_BUILD_JOBS is passed through when set.

  Permanent, never deleted by this script: claude-history-gui-target-main (the seed of every
  worktree volume) and the shared caches claude-history-gui-cargo-home and -ort-cache. Losing
  them costs a cold build and the downloads again. The image is rebuilt automatically when it is
  missing.

  Options (before the cargo arguments):
    --update        pull the newest rust:1-trixie, rebuild this checkout's image and remove the
                    image it replaced and the unused images of other toolchains (once at the start
                    of a session); alone, it runs no cargo command
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
. (Join-Path $PSScriptRoot 'common.ps1')

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
$repository = "$prefix-linux-dev"
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
$main = Get-MainCheckout $root
$isMain = [string]::Equals($root.TrimEnd('\'), $main.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)
if ($isMain) {
    $checkout = 'main'
} else {
    $leaf = ((Split-Path -Leaf $root).ToLowerInvariant() -replace '[^a-z0-9_.-]', '-')
    $checkout = "$leaf-$(Get-PathHash $root)"
}
$volume = "$prefix-target-$checkout"

# Exists only while a volume is being seeded: left behind, it marks a copy that was killed halfway.
function Get-SeedMarker([string]$Name) { Join-Path $env:LOCALAPPDATA "$prefix-dev\$Name.seeding" }

if ($gc) {
    foreach ($name in (Invoke-Utf8 { docker volume ls -q --filter "label=$prefix.worktree" })) {
        if ($name -eq $mainVolume) { continue }
        $path = Invoke-Utf8 { docker volume inspect --format "{{ index .Labels `"$prefix.worktree`" }}" $name }
        if ($path -and -not (Test-Path -LiteralPath $path)) {
            docker volume rm $name *> $null
            if ($LASTEXITCODE -eq 0) {
                Remove-Item -LiteralPath (Get-SeedMarker $name) -ErrorAction SilentlyContinue
                Write-Host "Removed $name ($path no longer exists)."
            } else { Write-Warning "$name is in use; kept." }
        }
    }
}
if ($drop) {
    if ($isMain) { Write-Warning "$volume is the seed of every worktree volume and is kept; --drop-volume is for worktrees." }
    elseif (Test-Volume $volume) {
        Invoke-Docker volume rm $volume | Out-Null
        Remove-Item -LiteralPath (Get-SeedMarker $volume) -ErrorAction SilentlyContinue
        Write-Host "Removed $volume."
    }
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
# Mount points owned by dev: a new named volume takes over the owner of its mount point. CARGO_HOME
# moves out of /usr/local/cargo (whose bin/ keeps the rustup proxies on PATH) into its own volume.
RUN useradd -m -u 1000 dev \
 && mkdir -p /target /home/dev/.cargo /home/dev/.cache/ort.pyke.io \
 && chown dev:dev /target \
 && chown -R dev:dev /home/dev/.cargo /home/dev/.cache
ENV CARGO_HOME=/home/dev/.cargo
USER dev
'@
# One tag per toolchain and image definition: checkouts on different toolchains (a branch that
# bumps rust-toolchain.toml) build different tags, so a run never starts another checkout's image,
# which would lack its toolchain (and dev cannot install one into the root-owned rustup home).
$image = "${repository}:$toolchain-$(Get-ShortHash "$dockerfile`n$components")"
# Without provenance attestations (they carry build timestamps) a cached rebuild yields the same
# image ID, so the tag does not move on every run.
$build = @('build', '--provenance=false', '-t', $image, '--build-arg', "TOOLCHAIN=$toolchain", '--build-arg', "COMPONENTS=$components")
if ($update) {
    $dockerfile | docker @build --pull - | Out-Host
} else {
    $dockerfile | docker @build -q - | Out-Null
}
if ($LASTEXITCODE -ne 0) { throw "docker build failed with exit code $LASTEXITCODE." }
if ($update) {
    # The image this rebuild replaced, then the images of other toolchains or image definitions
    # (old tags after a bump): a checkout that still uses one rebuilds it from the build cache, and
    # docker rmi refuses an image a container uses.
    docker image prune -f --filter "label=$prefix.dev=1" | Out-Host
    foreach ($other in (docker image ls $repository --format '{{.Repository}}:{{.Tag}}')) {
        if ($other -ne $image) {
            docker rmi $other *> $null
            if ($LASTEXITCODE -eq 0) { Write-Host "Removed $other." }
        }
    }
}
if (-not $cargoArgs.Count) { exit 0 }

$runId = Get-Random -Maximum 1000000

# This checkout's target volume; a new worktree volume starts as a copy of the main one without
# the workspace crates, incremental/ and examples/. The lock makes a concurrent run for the same
# checkout wait until the volume is ready instead of building in a half-copied one; the marker
# makes the next run redo a copy that was killed halfway (an existing volume is otherwise taken
# as ready).
Invoke-Locked "$volume-seed" {
    $marker = Get-SeedMarker $volume
    if ((Test-Path -LiteralPath $marker) -and (Test-Volume $volume)) {
        Write-Host "The last seeding of $volume was interrupted; seeding it again."
        # The copy of the killed run may still be going.
        docker ps -aq --filter "name=$prefix-seed-$checkout-" | ForEach-Object { docker rm -f $_ *> $null }
        docker volume rm $volume *> $null
        if ($LASTEXITCODE -ne 0) { throw "$volume is in use by a running build; run the script again when it has finished." }
    }
    if (-not (Test-Volume $volume)) {
        Invoke-Docker volume create --label "$prefix.worktree=$root" $volume | Out-Null
        if (-not $isMain -and (Test-Volume $mainVolume)) {
            Write-Host "Seeding $volume from $mainVolume..."
            New-Item -ItemType File -Force -Path $marker | Out-Null
            # pipefail: a failed read (e.g. "file changed as we read it" while the main checkout
            # builds) fails the seed even when the extraction succeeds.
            $copy = "set -o pipefail; tar -C /from --exclude=./debug/incremental --exclude=./debug/examples " +
                "--exclude='*claude_history*' --exclude='*claude-history*' -cf - . | tar -C /target -xf -"
            docker run --rm --pull never --name "$prefix-seed-$checkout-$runId" --label "$prefix.dev=1" `
                --mount "type=volume,src=$mainVolume,dst=/from,readonly" `
                --mount "type=volume,src=$volume,dst=/target" $image bash -c $copy
            if ($LASTEXITCODE -ne 0) {
                $code = $LASTEXITCODE
                docker volume rm $volume *> $null
                if ($LASTEXITCODE -eq 0) { Remove-Item -LiteralPath $marker }
                throw "Seeding $volume failed with exit code $code; run the script again to redo it."
            }
            Remove-Item -LiteralPath $marker
        }
    }
}

# --pull never: a local image only (it is never on a registry). In the moment between the build
# above and this start, --update from a checkout on another toolchain may remove it; the run then
# fails with "No such image" and succeeds when repeated.
$run = @('run', '--rm', '--init', '--pull', 'never', '--name', "$prefix-cargo-$checkout-$runId", '--label', "$prefix.dev=1",
    '-w', '/work',
    '--mount', "type=bind,src=$root,dst=/work",
    '--mount', "type=volume,src=$volume,dst=/target",
    '--mount', "type=volume,src=$prefix-cargo-home,dst=/home/dev/.cargo",
    '--mount', "type=volume,src=$prefix-ort-cache,dst=/home/dev/.cache/ort.pyke.io",
    '-e', 'CARGO_TARGET_DIR=/target', '-e', 'RUST_BACKTRACE=1')
if ($env:CARGO_BUILD_JOBS) { $run += @('-e', "CARGO_BUILD_JOBS=$env:CARGO_BUILD_JOBS") }
docker @run $image cargo @cargoArgs
exit $LASTEXITCODE
