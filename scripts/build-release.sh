#!/bin/sh
# Build brrr's release binaries. Each operating system gets its own
# native build; run this on (or for) the system named:
#
#   scripts/build-release.sh linux     bin/brrr-$(uname -m): static (musl), for
#                                      the Omarchy plugin and any other Linux system
#   scripts/build-release.sh windows   dist/brrr-windows-$(arch).exe
#   scripts/build-release.sh windows-installer
#                                      dist/brrr-windows-setup.exe, from both
#                                      Windows binaries already in dist/ (Inno Setup)
#   scripts/build-release.sh macos     dist/brrr.app, a universal (arm64 + x86_64)
#                                      menu bar app, and dist/brrr-macos.dmg
#
# No CPU-specific flags: the binaries run on any machine of their architecture.
set -eu
cd "$(dirname -- "$0")/.."
os=${1:?usage: scripts/build-release.sh linux|windows|windows-installer|macos}

# BRRR_ARCH overrides the detected architecture (x86_64 or aarch64), e.g.
# where an emulated shell reports x86_64 on an ARM machine.
case ${BRRR_ARCH:-$(uname -m)} in
  x86_64 | amd64 | AMD64) arch=x86_64 ;;
  aarch64 | arm64 | ARM64) arch=aarch64 ;;
  *) echo "unsupported architecture: ${BRRR_ARCH:-$(uname -m)}" >&2; exit 1 ;;
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
    out=bin/brrr-$arch
    cp "target/$target/release/brrr" "$out"
    chmod 755 "$out"
    smoke "$out"
    ;;
  windows)
    target=$arch-pc-windows-msvc
    cargo build --release --locked --target "$target"
    mkdir -p dist
    out=dist/brrr-windows-$arch.exe
    cp "target/$target/release/brrr.exe" "$out"
    smoke "$out"
    # Clean Windows installs lack the Visual C++ runtime; .cargo/config.toml
    # links it statically, so the .exe must not import it.
    if grep -qai 'vcruntime140' "$out"; then
      echo "$out: imports VCRUNTIME140.dll; the C runtime must be linked statically" >&2
      exit 1
    fi
    ;;
  macos)
    for target in aarch64-apple-darwin x86_64-apple-darwin; do
      cargo build --release --locked --target "$target"
    done
    app=dist/brrr.app
    rm -rf "$app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    lipo -create -output "$app/Contents/MacOS/brrr" \
      target/aarch64-apple-darwin/release/brrr target/x86_64-apple-darwin/release/brrr
    version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)

    # The glass app icon (brrr-macos.svg), at every size Finder and the Dock ask for.
    iconset=dist/brrr.iconset
    rm -rf "$iconset"
    mkdir -p "$iconset"
    for size in 16 32 128 256 512; do
      sips -z "$size" "$size" assets/icon/brrr-macos-1024.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
      double=$((size * 2))
      sips -z "$double" "$double" assets/icon/brrr-macos-1024.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
    done
    iconutil -c icns "$iconset" -o "$app/Contents/Resources/brrr.icns"
    rm -rf "$iconset"

    # LSUIElement: a menu bar app, without a Dock icon or app menu.
    cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>brrr</string>
  <key>CFBundleIconFile</key><string>brrr</string>
  <key>CFBundleIdentifier</key><string>com.brrr.brrr</string>
  <key>CFBundleName</key><string>brrr</string>
  <key>CFBundleDisplayName</key><string>brrr</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHumanReadableCopyright</key><string>MIT License</string>
</dict>
</plist>
PLIST
    # Ad-hoc signature, after every file is in place: Apple Silicon refuses
    # to run unsigned code.
    codesign --force --sign - "$app"
    smoke "$app/Contents/MacOS/brrr"

    # A disk image with the app beside a link to Applications, so installing
    # is a drag.
    staging=dist/dmg
    rm -rf "$staging"
    mkdir -p "$staging"
    cp -R "$app" "$staging/"
    ln -s /Applications "$staging/Applications"
    dmg=dist/brrr-macos.dmg
    rm -f "$dmg"
    hdiutil create -volname "brrr $version" -srcfolder "$staging" -fs HFS+ -format UDZO -ov "$dmg"
    hdiutil verify "$dmg"
    rm -rf "$staging"
    out="$app and $dmg"
    ;;
  windows-installer)
    # Inno Setup's compiler; the default install location is tried too.
    iscc=${ISCC:-}
    if [ -z "$iscc" ]; then
      if command -v iscc >/dev/null 2>&1; then iscc=iscc
      elif [ -x "/c/Program Files (x86)/Inno Setup 6/ISCC.exe" ]; then iscc="/c/Program Files (x86)/Inno Setup 6/ISCC.exe"
      else echo "Inno Setup's ISCC not found; set ISCC" >&2; exit 1
      fi
    fi
    for each in x86_64 aarch64; do
      [ -f "dist/brrr-windows-$each.exe" ] || { echo "missing dist/brrr-windows-$each.exe" >&2; exit 1; }
    done
    version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
    # The script's paths are relative to itself (dist/ by default). Keep
    # Git Bash from rewriting the /D option into a path.
    MSYS2_ARG_CONV_EXCL='*' "$iscc" "/DAppVersion=$version" installer/windows/brrr.iss
    out=dist/brrr-windows-setup.exe
    [ -f "$out" ] || { echo "Inno Setup did not produce $out" >&2; exit 1; }
    ;;
  *) echo "usage: scripts/build-release.sh linux|windows|windows-installer|macos" >&2; exit 2 ;;
esac
echo "built $out"
