#!/usr/bin/env bash
# Builds "Claude History.app" and its zip (DESIGN sections 14.5, 16.3).
#
#   packaging/macos/make-app.sh <binary> <onnxruntime-dir> <version> <out-dir>
#
#   <binary>            target/aarch64-apple-darwin/release/claude-history-gui (built with
#                       --features release-dynamic-ort)
#   <onnxruntime-dir>   the extracted official ONNX Runtime (osx-arm64); its lib/libonnxruntime*
#                       files go to Contents/MacOS/lib, where core looks for them (<exe dir>/lib)
#   <version>           the Cargo version (Info.plist uses it)
#   <out-dir>           receives claude-history-gui-v<version>-macos-aarch64.zip (+ .sha256)
#
# The bundle is ad-hoc signed (codesign --force --deep -s -). Notarization is out of scope: users
# open it with right-click > Open the first time. CHG_PACKAGE_STAGE_ONLY=1 stops after assembling
# the bundle (no codesign, no ditto): used to test the layout on machines that are not a Mac.
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

app_name="Claude History.app"
artifact="claude-history-gui-v${version}-macos-aarch64"
mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
app="$stage/$app_name"

mkdir -p "$app/Contents/MacOS/lib" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/claude-history-gui"
chmod +x "$app/Contents/MacOS/claude-history-gui"
# Keep the symlinks (libonnxruntime.dylib -> libonnxruntime.<ver>.dylib).
find "$ort_dir/lib" -maxdepth 1 \( -type f -o -type l \) -name 'libonnxruntime*' -exec cp -P {} "$app/Contents/MacOS/lib/" \;
ls "$app"/Contents/MacOS/lib/libonnxruntime*.dylib >/dev/null
sed "s/@VERSION@/${version}/g" "$here/Info.plist.in" > "$app/Contents/Info.plist"
cp "$repo/crates/gui/assets/app.icns" "$app/Contents/Resources/app.icns"
cp "$repo/LICENSE" "$app/Contents/Resources/LICENSE"
cp "$repo/crates/core/LICENSE-MIT" "$app/Contents/Resources/LICENSE-MIT"

if [ "${CHG_PACKAGE_STAGE_ONLY:-0}" = "1" ]; then
    rm -rf "$out_dir/$app_name"
    cp -R "$app" "$out_dir/$app_name"
    echo "staged $out_dir/$app_name"
    exit 0
fi

codesign --force --deep -s - "$app"
codesign --verify --deep --strict "$app"
(cd "$stage" && ditto -c -k --keepParent "$app_name" "$out_dir/${artifact}.zip")
(cd "$out_dir" && shasum -a 256 "${artifact}.zip" > "${artifact}.zip.sha256")
echo "$out_dir/${artifact}.zip"
