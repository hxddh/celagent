#!/usr/bin/env bash
# Package Lenslet for macOS as one universal (arm64 + x86_64) build:
#   Lenslet-VERSION.dmg                         the app, with the CLI inside
#   lenslet-cli-VERSION-universal-macos.tar.gz  the CLI alone (install.sh, CI)
#
# Run on macOS after:
#   cargo build --release --locked --target aarch64-apple-darwin -p lenslet -p lenslet-ui
#   cargo build --release --locked --target x86_64-apple-darwin  -p lenslet -p lenslet-ui
#
# The app is ad-hoc signed, not notarized: on first open, macOS asks once
# (System Settings > Privacy & Security > Open Anyway). Notarizing needs an
# Apple Developer ID and is not set up yet.
set -euo pipefail

VERSION="${1:?usage: package-macos.sh VERSION [OUT_DIR]}"
OUT="${2:-dist}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STAGE="$OUT/stage"
APP="$STAGE/Lenslet.app"

rm -rf "$STAGE" "$OUT/AppIcon.iconset"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$OUT/AppIcon.iconset"

universal() {
  lipo -create \
    "$ROOT/target/aarch64-apple-darwin/release/$1" \
    "$ROOT/target/x86_64-apple-darwin/release/$1" \
    -output "$2"
}
# The window is `lenslet-ui` and the CLI `lenslet`: macOS volumes are usually
# case-insensitive, so they cannot be `Lenslet` and `lenslet` side by side.
universal lenslet-ui "$APP/Contents/MacOS/lenslet-ui"
universal lenslet "$APP/Contents/MacOS/lenslet"
lipo -info "$APP/Contents/MacOS/lenslet-ui" "$APP/Contents/MacOS/lenslet"

for spec in "16 16x16" "32 16x16@2x" "32 32x32" "64 32x32@2x" "128 128x128" "256 128x128@2x" "256 256x256" "512 256x256@2x" "512 512x512" "1024 512x512@2x"; do
  set -- $spec
  sips -z "$1" "$1" "$ROOT/assets/app-icon.png" --out "$OUT/AppIcon.iconset/icon_$2.png" >/dev/null
done
iconutil -c icns "$OUT/AppIcon.iconset" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "$OUT/AppIcon.iconset"
cp "$ROOT/LICENSE" "$APP/Contents/Resources/"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Lenslet</string>
  <key>CFBundleDisplayName</key><string>Lenslet</string>
  <key>CFBundleIdentifier</key><string>dev.lenslet.app</string>
  <key>CFBundleExecutable</key><string>lenslet-ui</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST

codesign --force --sign - "$APP/Contents/MacOS/lenslet"
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"

# The disk image: the app and a shortcut to drag it onto.
ln -s /Applications "$STAGE/Applications"
DMG="$OUT/Lenslet-${VERSION}.dmg"
rm -f "$DMG"
hdiutil create -volname "Lenslet ${VERSION}" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
hdiutil verify "$DMG"

# The CLI alone.
CLI="$OUT/cli"
rm -rf "$CLI" && mkdir -p "$CLI"
cp "$APP/Contents/MacOS/lenslet" "$CLI/lenslet"
tar -C "$CLI" -czf "$OUT/lenslet-cli-${VERSION}-universal-macos.tar.gz" lenslet
rm -rf "$CLI" "$STAGE"

ls -lh "$OUT"
