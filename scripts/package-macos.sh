#!/usr/bin/env bash
# Build a relocatable, ad-hoc signed Lenslet.app (window) plus the lenslet CLI.
# Run on macOS after `cargo build --release -p lenslet -p lenslet-ui`.
# Layout: lenslet (CLI) next to Lenslet.app (window); `lenslet ui` opens it.
set -euo pipefail

VERSION="${1:?usage: package-macos.sh VERSION [OUT_DIR]}"
OUT="${2:-dist}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ARCH="$(uname -m)"
NAME="lenslet-${VERSION}-macos-${ARCH}"
APP="${OUT}/${NAME}/Lenslet.app"
ICONSET="${OUT}/AppIcon.iconset"

rm -rf "${OUT:?}/${NAME}" "$ICONSET"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$ICONSET" "${OUT}/${NAME}/assets/fonts" "${OUT}/${NAME}/docs"

cp "$ROOT/target/release/lenslet-ui" "$APP/Contents/MacOS/Lenslet"
cp "$ROOT/target/release/lenslet" "${OUT}/${NAME}/lenslet"
cp "$ROOT/README.md" "$ROOT/RELEASE_NOTES.md" "$ROOT/LICENSE" "${OUT}/${NAME}/"
cp -r "$ROOT/docs/." "${OUT}/${NAME}/docs/"
cp "$ROOT/assets/fonts/LICENSE-"* "${OUT}/${NAME}/assets/fonts/"

# A matched icon makes the macOS app feel native instead of a generic binary.
for spec in "16 16x16" "32 16x16@2x" "32 32x32" "64 32x32@2x" "128 128x128" "256 128x128@2x" "256 256x256" "512 256x256@2x" "512 512x512" "1024 512x512@2x"; do
  set -- $spec
  sips -z "$1" "$1" "$ROOT/assets/app-icon.png" --out "$ICONSET/icon_$2.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "$ICONSET"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Lenslet</string>
  <key>CFBundleDisplayName</key><string>Lenslet</string>
  <key>CFBundleIdentifier</key><string>dev.celagent.lenslet</string>
  <key>CFBundleExecutable</key><string>Lenslet</string>
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

codesign --force --deep --sign - "$APP"
codesign --force --sign - "${OUT}/${NAME}/lenslet"
tar -C "$OUT" -czf "${OUT}/${NAME}.tar.gz" "$NAME"
(cd "$OUT" && shasum -a 256 "${NAME}.tar.gz" > SHA256SUMS-macos)
file "$APP/Contents/MacOS/Lenslet"
echo "$OUT/${NAME}.tar.gz"
