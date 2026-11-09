#!/bin/sh
# Build Omapower's release binaries. Each operating system gets its own
# native build; run this on (or for) the system named:
#
#   scripts/build-release.sh linux     bin/omapower-$(uname -m): static (musl), for
#                                      the Omarchy plugin and any other Linux system
#   scripts/build-release.sh windows   dist/omapower-windows-$(arch).exe
#   scripts/build-release.sh macos     dist/Omapower.app, a universal (arm64 + x86_64)
#                                      menu bar app
#
# No CPU-specific flags: the binaries run on any machine of their architecture.
set -eu
cd "$(dirname -- "$0")/.."
os=${1:?usage: scripts/build-release.sh linux|windows|macos}

case $(uname -m) in
  x86_64 | amd64 | AMD64) arch=x86_64 ;;
  aarch64 | arm64 | ARM64) arch=aarch64 ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

# Argument errors exit 2 before touching anything: a cheap smoke test.
smoke() {
  status=0
  "$1" --bogus 2>/dev/null || status=$?
  [ "$status" -eq 2 ] || { echo "$1: smoke test failed (status $status)" >&2; exit 1; }
}

case $os in
  linux)
    target=$arch-unknown-linux-musl
    cargo build --release --locked --target "$target"
    out=bin/omapower-$arch
    cp "target/$target/release/omapower" "$out"
    chmod 755 "$out"
    smoke "$out"
    ;;
  windows)
    target=$arch-pc-windows-msvc
    cargo build --release --locked --target "$target"
    mkdir -p dist
    out=dist/omapower-windows-$arch.exe
    cp "target/$target/release/omapower.exe" "$out"
    smoke "$out"
    ;;
  macos)
    for target in aarch64-apple-darwin x86_64-apple-darwin; do
      cargo build --release --locked --target "$target"
    done
    app=dist/Omapower.app
    rm -rf "$app"
    mkdir -p "$app/Contents/MacOS"
    lipo -create -output "$app/Contents/MacOS/omapower" \
      target/aarch64-apple-darwin/release/omapower target/x86_64-apple-darwin/release/omapower
    version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
    # LSUIElement: a menu bar app, without a Dock icon or app menu.
    cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>omapower</string>
  <key>CFBundleIdentifier</key><string>com.omapower.Omapower</string>
  <key>CFBundleName</key><string>Omapower</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSUIElement</key><true/>
</dict>
</plist>
PLIST
    # Ad-hoc signature: Apple Silicon refuses to run unsigned code.
    codesign --force --sign - "$app"
    smoke "$app/Contents/MacOS/omapower"
    out=$app
    ;;
  *) echo "usage: scripts/build-release.sh linux|windows|macos" >&2; exit 2 ;;
esac
echo "built $out"
