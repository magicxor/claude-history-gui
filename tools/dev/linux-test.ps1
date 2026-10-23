<#
.SYNOPSIS
  Runs cargo on Linux (Docker) for this checkout, with all build state in named volumes.

.DESCRIPTION
  Builds or reuses the local image claude-history-gui-linux-dev:<toolchain>-<hash>: rust:1-trixie
  with the Linux packages of .github/workflows/ci.yml and the toolchain of rust-toolchain.toml
  (channel, profile, components, targets), running as the non-root user dev (uid 1000), so the
  permission tests of test_support::fault run as on CI instead of skipping as root. The tag differs
  per rust-toolchain.toml and image definition, so checkouts on different toolchains never run
  each other's image. Then runs `cargo <arguments>` in a throw-away container (--rm) named
  claude-history-gui-cargo-<checkout>-<random>:

    /work                               this checkout (bind mount)
    /target (CARGO_TARGET_DIR)          volume claude-history-gui-target-main-<hash> for the main
                                        checkout (one per clone of the repository),
                                        claude-history-gui-target-wt-<folder>-<hash> for a worktree
    CARGO_HOME (registry, git, and      shared volume claude-history-gui-cargo-home: one volume, so
    the locks that guard them)          concurrent containers share cargo's cache locks
    the ONNX Runtime download (ort-sys) shared volume claude-history-gui-ort-cache

  Everything this script creates is named claude-history-gui-* and labelled claude-history-gui.dev
  (image, containers) or claude-history-gui.worktree=<checkout path> (target volumes). Nothing
  stays behind in containers or anonymous volumes: the Docker disk grows by these volumes, the
  image and BuildKit's build cache (which Docker's builder garbage collection keeps bounded).
  A worktree's new target volume is seeded from its clone's main volume: debug/ only, without the
  workspace's own output, timestamps kept. Its first build compiles only the workspace crates;
  the paths are the same in every container, so the seed is reused as is. Concurrent runs for the
  same checkout wait until its volume is ready, and a copy that was killed halfway is redone (lock
  files and seed markers in .git\claude-history-gui-dev). Build in the main checkout once to
  create the seed; a seed from an older main checkout is slower, never wrong.
  CARGO_BUILD_JOBS is passed through when set.

  Permanent: the main volume of every existing clone (the seed of its worktree volumes) and the
  shared caches claude-history-gui-cargo-home and -ort-cache, which this script never deletes.
  Losing them costs a cold build and the downloads again. The image is rebuilt automatically when
  it is missing.

  Options (before the cargo arguments):
    --update        pull the newest rust:1-trixie, rebuild this checkout's image and remove the
                    image it replaced (once at the start of a session); alone, it runs no cargo
                    command
    --prune-images  remove the images of other toolchains or image definitions (old tags after a
                    bump) that no container uses. Only while no other checkout is starting a Linux
                    build: a run that has checked its tag but not yet started its container would
                    fail with "No such image". A checkout that still needs a removed tag rebuilds
                    it from the build cache.
    --drop-volume   delete this worktree's target volume (when the worktree is done); refused in
                    the main checkout
    --gc            delete the target volumes of worktrees and clones that no longer exist at
                    their path (this clone's main volume is always kept) and target volumes without
                    a checkout label, then the markers and locks of volumes and worktrees that are
                    gone. Stops without changes when Docker does not answer.

  From a PowerShell prompt, quote the separator before clippy or test-binary arguments ('--'):
  PowerShell drops a bare -- when it calls a script; pwsh -File passes it on.

.EXAMPLE
  pwsh -NoProfile -File tools/dev/linux-test.ps1 test -p claude-history-core --locked
.EXAMPLE
  pwsh -NoProfile -File tools/dev/linux-test.ps1 clippy --workspace --all-targets --locked -- -D warnings
.EXAMPLE
  pwsh -NoProfile -File tools/dev/linux-test.ps1 --update
#>
#Requires -Version 7.3
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'common.ps1')

$update = $false; $pruneImages = $false; $drop = $false; $gc = $false
$rest = @($args)
while ($rest.Count -gt 0 -and $rest[0] -in '--update', '--prune-images', '--drop-volume', '--gc') {
    switch ($rest[0]) {
        '--update' { $update = $true }
        '--prune-images' { $pruneImages = $true }
        '--drop-volume' { $drop = $true }
        '--gc' { $gc = $true }
    }
    $rest = @($rest | Select-Object -Skip 1)
}
$cargoArgs = $rest
if (-not ($update -or $pruneImages -or $drop -or $gc -or $cargoArgs.Count)) {
    throw 'Nothing to do: pass cargo arguments or --update / --prune-images / --drop-volume / --gc (see Get-Help).'
}

# Every Docker object of this script starts with this prefix, so it cannot be mistaken for the
# user's own images, containers and volumes.
$prefix = 'claude-history-gui'
$repository = "$prefix-linux-dev"
$root = Get-CheckoutRoot

function Invoke-Docker {
    docker @args
    if ($LASTEXITCODE -ne 0) { throw "docker $($args[0]) failed with exit code $LASTEXITCODE." }
}

# All volume names. A failed listing stops the script: it must never read as "the volume is gone"
# (that would, for one, drop the marker of a half-filled volume).
function Get-VolumeNames {
    $names = Invoke-Utf8 { docker volume ls --format '{{.Name}}' }
    if ($LASTEXITCODE -ne 0) { throw 'Docker does not answer (docker volume ls failed); nothing was changed.' }
    @($names)
}

# This checkout's name in the volume and container names: main-<hash of its path> for the main
# checkout (first `git worktree list` entry; the hash keeps two clones of the repository apart),
# wt-<folder>-<hash of its path> for a worktree, so that no worktree can look like a main volume.
$main = Get-MainCheckout $root
$isMain = Test-SamePath $root $main
$mainVolume = "$prefix-target-main-$(Get-PathHash $main)"
if ($isMain) {
    $checkout = "main-$(Get-PathHash $main)"
} else {
    $leaf = ((Split-Path -Leaf $root).ToLowerInvariant() -replace '[^a-z0-9_.-]', '-')
    $checkout = "wt-$leaf-$(Get-PathHash $root)"
}
$volume = "$prefix-target-$checkout"

# The lock file and the seed marker of a volume, in this clone's .git.
$stateDir = Get-StateDir $root
function Get-LockPath([string]$Name) { Join-Path $stateDir "$Name.lock" }
function Get-SeedMarker([string]$Name) { Join-Path $stateDir "$Name.seeding" }

if ($gc) {
    $label = "$prefix.worktree"
    $listing = Invoke-Utf8 { docker volume ls --filter "name=$prefix-target-" --format "{{.Name}}`t{{.Label `"$label`"}}" }
    if ($LASTEXITCODE -ne 0) { throw 'Docker does not answer (docker volume ls failed); nothing was removed.' }
    foreach ($line in $listing) {
        $name, $path = $line -split "`t", 2
        if ($name -eq $mainVolume -or ($path -and (Test-Path -LiteralPath $path))) { continue }
        # A target volume without the label was created by docker itself (a run mounting a volume
        # that was removed in the meantime); nothing refers to it.
        docker volume rm $name *> $null
        if ($LASTEXITCODE -ne 0) { Write-Warning "$name is in use; kept." }
        elseif ($path) { Write-Host "Removed $name ($path no longer exists)." }
        else { Write-Host "Removed $name (no checkout label)." }
    }
    # Markers of volumes that are gone, each under its volume's lock: a seed writes its marker
    # before it creates the volume and holds the lock until the copy is done.
    foreach ($marker in @(Get-ChildItem -LiteralPath $stateDir -Filter '*.seeding' -ErrorAction SilentlyContinue)) {
        Invoke-Locked (Get-LockPath $marker.BaseName) {
            if ((Get-VolumeNames) -notcontains $marker.BaseName) { Remove-Item -LiteralPath $marker.FullName }
        }
    }
    # Lock files of volumes that are gone and of seed-target.ps1 for worktrees that are gone (a lock
    # that is held cannot be deleted and stays).
    $volumes = Get-VolumeNames
    $worktreeLocks = @(Get-Worktrees $root | ForEach-Object { "seed-target-$(Get-PathHash $_)" })
    Get-ChildItem -LiteralPath $stateDir -Filter '*.lock' -ErrorAction SilentlyContinue |
        Where-Object {
            ($_.BaseName -like "$prefix-target-*" -and $volumes -notcontains $_.BaseName) -or
            ($_.BaseName -like 'seed-target-*' -and $worktreeLocks -notcontains $_.BaseName)
        } |
        ForEach-Object { Remove-Item -LiteralPath $_.FullName -ErrorAction SilentlyContinue }
}
if ($drop) {
    if ($isMain) {
        Write-Warning "$volume is the seed of every worktree volume and is kept; --drop-volume is for worktrees."
    } else {
        # Under the volume's lock, so that it never removes a volume that is being seeded.
        Invoke-Locked (Get-LockPath $volume) {
            if ((Get-VolumeNames) -contains $volume) {
                Invoke-Docker volume rm $volume | Out-Null
                Write-Host "Removed $volume."
            } else {
                Write-Host "$volume does not exist."
            }
            Remove-Item -LiteralPath (Get-SeedMarker $volume) -ErrorAction SilentlyContinue
        }
    }
}
if (-not ($update -or $pruneImages -or $cargoArgs.Count)) { exit 0 }

# The image: built when its tag is missing, rebuilt from the newest base image with --update.
$toolchainFile = (Get-Content -Raw -LiteralPath (Join-Path $root 'rust-toolchain.toml')) -replace "`r`n", "`n"
function Get-ToolchainList([string]$Key) {
    if ($toolchainFile -match "(?m)^$Key\s*=\s*\[([^\]]*)\]") { $Matches[1] -replace '[\s"]', '' } else { '' }
}
if (-not ($toolchainFile -match '(?m)^channel\s*=\s*"([^"]+)"')) { throw 'rust-toolchain.toml has no channel.' }
$toolchain = $Matches[1]
$rustupProfile = if ($toolchainFile -match '(?m)^profile\s*=\s*"([^"]+)"') { $Matches[1] } else { 'minimal' }
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
ARG PROFILE
ARG COMPONENTS
ARG TARGETS
RUN rustup toolchain install "$TOOLCHAIN" --profile "$PROFILE" \
    ${COMPONENTS:+--component "$COMPONENTS"} ${TARGETS:+--target "$TARGETS"} \
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
# One tag per rust-toolchain.toml and image definition: checkouts on different toolchains (a branch
# that bumps rust-toolchain.toml) build different tags, so a run never starts another checkout's
# image, which would lack its toolchain (and dev cannot install one into the root-owned rustup
# home).
$image = "${repository}:$toolchain-$(Get-ShortHash "$dockerfile`n$toolchainFile")"
# Without provenance attestations (they carry build timestamps) a cached rebuild yields the same
# image ID, so the tag does not move on every run.
$build = @('build', '--provenance=false', '-t', $image, '--build-arg', "TOOLCHAIN=$toolchain",
    '--build-arg', "PROFILE=$rustupProfile", '--build-arg', "COMPONENTS=$(Get-ToolchainList 'components')",
    '--build-arg', "TARGETS=$(Get-ToolchainList 'targets')")
if ($update -or $cargoArgs.Count) {
    # The tag pins rust-toolchain.toml and the image definition, so an existing tag needs no build.
    docker image inspect $image *> $null
    if ($update) {
        $dockerfile | docker @build --pull - | Out-Host
        if ($LASTEXITCODE -ne 0) { throw "docker build failed with exit code $LASTEXITCODE." }
        # The image this rebuild replaced, now without a tag. Safe while other runs go on: they
        # start their containers by tag, which already names the new image.
        docker image prune -f --filter "label=$prefix.dev=1" | Out-Host
    } elseif ($LASTEXITCODE -ne 0) {
        $dockerfile | docker @build -q - | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "docker build failed with exit code $LASTEXITCODE." }
    }
}
if ($pruneImages) {
    # Tags of other configurations. Not part of --update: another checkout's run may have checked
    # its tag and not yet started its container (docker only protects images that containers use).
    foreach ($other in (docker image ls $repository --format '{{.Repository}}:{{.Tag}}')) {
        if ($other -ne $image) {
            docker rmi $other *> $null
            if ($LASTEXITCODE -eq 0) { Write-Host "Removed $other." } else { Write-Host "Kept $other (a container uses it)." }
        }
    }
}
if (-not $cargoArgs.Count) { exit 0 }

$runId = Get-Random -Maximum 1000000

# This checkout's target volume (Invoke-Seed: lock, marker, redo of a copy killed halfway). A new
# worktree volume starts as a copy of the main one: debug/ only, without incremental/, examples/
# and the workspace's own output.
Invoke-Seed -Name $volume -LockPath (Get-LockPath $volume) -Marker (Get-SeedMarker $volume) `
    -Exists { (Get-VolumeNames) -contains $volume } `
    -Remove {
        # The copy of a killed run may still be going.
        docker ps -aq --filter "name=$prefix-seed-$checkout-" | ForEach-Object { docker rm -f $_ *> $null }
        docker volume rm $volume *> $null
        if ($LASTEXITCODE -ne 0) { throw "$volume is in use by a running build; run the script again when it has finished." }
    } `
    -Create {
        Invoke-Docker volume create --label "$prefix.worktree=$root" $volume | Out-Null
        if ($isMain -or (Get-VolumeNames) -notcontains $mainVolume) { return }
        Write-Host "Seeding $volume from $mainVolume..."
        $excludes = @('--exclude=./debug/incremental', '--exclude=./debug/examples') +
            @(Get-WorkspaceOutputPatterns $root | ForEach-Object { "--exclude=$_" })
        # pipefail: a failed read (e.g. "file changed as we read it" while the main checkout builds)
        # fails the seed even when the extraction succeeds. --init: Ctrl+C reaches tar.
        $copy = 'set -o pipefail; [ -d /from/debug ] || exit 0; tar -C /from "$@" -cf - ./debug | tar -C /target -xf -'
        docker run --rm --init --pull never --name "$prefix-seed-$checkout-$runId" --label "$prefix.dev=1" `
            --mount "type=volume,src=$mainVolume,dst=/from,readonly" `
            --mount "type=volume,src=$volume,dst=/target" $image bash -c $copy bash @excludes
        if ($LASTEXITCODE -ne 0) { throw "Seeding $volume failed with exit code $LASTEXITCODE; run the script again to redo it." }
    } | Out-Null

# --pull never: a local image only (it is never on a registry); if --prune-images removed it after
# the check above, the run fails with "No such image" instead of trying a registry. The bind
# source is quoted: --mount is comma-separated, and a path may contain commas.
$run = @('run', '--rm', '--init', '--pull', 'never', '--name', "$prefix-cargo-$checkout-$runId", '--label', "$prefix.dev=1",
    '-w', '/work',
    '--mount', "type=bind,`"src=$root`",dst=/work",
    '--mount', "type=volume,src=$volume,dst=/target",
    '--mount', "type=volume,src=$prefix-cargo-home,dst=/home/dev/.cargo",
    '--mount', "type=volume,src=$prefix-ort-cache,dst=/home/dev/.cache/ort.pyke.io",
    '-e', 'CARGO_TARGET_DIR=/target', '-e', 'RUST_BACKTRACE=1')
if ($env:CARGO_BUILD_JOBS) { $run += @('-e', "CARGO_BUILD_JOBS=$env:CARGO_BUILD_JOBS") }
docker @run $image cargo @cargoArgs
exit $LASTEXITCODE
