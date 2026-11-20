#!/usr/bin/env bash
# Package Lenslet for Linux:
#   lenslet-VERSION-ARCH-linux.tar.gz           CLI + window (glibc >= 2.35)
#   lenslet-cli-VERSION-ARCH-linux-musl.tar.gz  static CLI (any distro, CI images)
#
# Run after:
#   cargo build --release --locked -p lenslet -p lenslet-ui
#   cargo build --release --locked -p lenslet --target ARCH-unknown-linux-musl
set -euo pipefail

VERSION="${1:?usage: package-linux.sh VERSION [OUT_DIR]}"
OUT="${2:-dist}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ARCH="$(uname -m)"
NAME="lenslet-${VERSION}-${ARCH}-linux"
mkdir -p "$OUT/$NAME/assets/fonts" \
         "$OUT/$NAME/share/applications" \
         "$OUT/$NAME/share/icons/hicolor/512x512/apps"

cp "$ROOT/target/release/lenslet" "$ROOT/target/release/lenslet-ui" "$OUT/$NAME/"
cp "$ROOT/README.md" "$ROOT/RELEASE_NOTES.md" "$ROOT/LICENSE" "$OUT/$NAME/"
cp "$ROOT/assets/fonts/LICENSE-"* "$OUT/$NAME/assets/fonts/"
cp "$ROOT/assets/app-icon.png" "$OUT/$NAME/share/icons/hicolor/512x512/apps/dev.lenslet.app.png"
cat > "$OUT/$NAME/share/applications/dev.lenslet.app.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=Lenslet
Comment=Local AWS SDK Flight Recorder & Least-Privilege IAM Workbench
Exec=lenslet-ui %f
Icon=dev.lenslet.app
Terminal=false
Categories=Development;Debugger;Cloud;
MimeType=application/jsonl;application/x-ndjson;
StartupWMClass=dev.lenslet.app
Keywords=AWS;IAM;LocalStack;Moto;S3;DynamoDB;Trace;
DESKTOP
strip "$OUT/$NAME/lenslet" "$OUT/$NAME/lenslet-ui"

# The binaries run where their glibc is at least the one they need.
for bin in lenslet lenslet-ui; do
  floor=$(objdump -T "$OUT/$NAME/$bin" | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1)
  echo "$bin needs $floor"
  test "$(printf '%s\n' "$floor" GLIBC_2.35 | sort -V | tail -1)" = GLIBC_2.35
done
tar -C "$OUT" -czf "$OUT/$NAME.tar.gz" "$NAME"
rm -rf "${OUT:?}/$NAME"

CLI="$OUT/cli-$ARCH"
mkdir -p "$CLI"
cp "$ROOT/target/${ARCH}-unknown-linux-musl/release/lenslet" "$CLI/lenslet"
strip "$CLI/lenslet"
file "$CLI/lenslet" | grep -q "static" || { echo "the musl CLI is not static"; exit 1; }
"$CLI/lenslet" --version
tar -C "$CLI" -czf "$OUT/lenslet-cli-${VERSION}-${ARCH}-linux-musl.tar.gz" lenslet
rm -rf "$CLI"

ls -lh "$OUT"
