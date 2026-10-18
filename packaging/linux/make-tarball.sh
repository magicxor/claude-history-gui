#!/usr/bin/env bash
# Builds the Linux release tarball (DESIGN sections 14.6, 16.3).
#
#   packaging/linux/make-tarball.sh <binary> <onnxruntime-dir> <version> <out-dir>
#
#   <binary>            target/release/claude-history-gui (built with --features release-dynamic-ort)
#   <onnxruntime-dir>   the extracted official ONNX Runtime (linux-x64); lib/libonnxruntime.so* is
#                       copied to lib/ next to the binary, where core looks for it (<exe dir>/lib)
#   <version>           the Cargo version
#   <out-dir>           receives claude-history-gui-v<version>-linux-x86_64.tar.gz (+ .sha256)
#
# Contents (inside the directory claude-history-gui-v<version>-linux-x86_64/): the binary, lib/,
# claude-history-gui.desktop, claude-history-gui.png (512 px), LICENSE, LICENSE-MIT, README.txt.
set -euo pipefail

if [ "$#" -ne 4 ]; then
    echo "usage: $0 <binary> <onnxruntime-dir> <version> <out-dir>" >&2
    exit 2
fi
binary="$1"; ort_dir="$2"; version="$3"; out_dir="$4"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"

test -f "$binary" || { echo "binary not found: $binary" >&2; exit 1; }
test -d "$ort_dir/lib" || { echo "ONNX Runtime lib dir not found: $ort_dir/lib" >&2; exit 1; }

artifact="claude-history-gui-v${version}-linux-x86_64"
mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
pkg="$stage/$artifact"
mkdir -p "$pkg/lib"

cp "$binary" "$pkg/claude-history-gui"
chmod +x "$pkg/claude-history-gui"
# Only strip where `strip` exists (the release job runs it on Linux).
if command -v strip >/dev/null 2>&1 && [ "${CHG_PACKAGE_NO_STRIP:-0}" != "1" ]; then
    strip "$pkg/claude-history-gui" || true
fi
find "$ort_dir/lib" -maxdepth 1 \( -type f -o -type l \) -name 'libonnxruntime*' -exec cp -P {} "$pkg/lib/" \;
ls "$pkg"/lib/libonnxruntime.so* >/dev/null
cp "$here/claude-history-gui.desktop" "$pkg/claude-history-gui.desktop"
cp "$repo/crates/gui/assets/app.png" "$pkg/claude-history-gui.png"
cp "$repo/LICENSE" "$pkg/LICENSE"
cp "$repo/crates/core/LICENSE-MIT" "$pkg/LICENSE-MIT"
cat > "$pkg/README.txt" <<EOF
Claude History ${version} (claude-history-gui), Linux x86_64

Run ./claude-history-gui (needs a Vulkan driver and an X11 or Wayland session).
lib/ holds the ONNX Runtime used by semantic search; keep it next to the binary.

To add it to the application menu:
  install -Dm644 claude-history-gui.desktop ~/.local/share/applications/claude-history-gui.desktop
  install -Dm644 claude-history-gui.png ~/.local/share/icons/hicolor/512x512/apps/claude-history-gui.png
and put the binary (with its lib/ directory) somewhere on PATH, or edit Exec= in the .desktop file.
EOF

tar -C "$stage" -czf "$out_dir/${artifact}.tar.gz" "$artifact"
(cd "$out_dir" && sha256sum "${artifact}.tar.gz" > "${artifact}.tar.gz.sha256")
echo "$out_dir/${artifact}.tar.gz"
