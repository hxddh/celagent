#!/bin/sh
# Install the lenslet CLI (or the full desktop app + CLI with --app).
#
#   curl -fsSL https://github.com/hxddh/lenslet/releases/latest/download/install.sh | sh
#   curl -fsSL https://github.com/hxddh/lenslet/releases/latest/download/install.sh | sh -s -- --app
#
# Options (or environment variables):
#   --app             LENSLET_INSTALL_APP  install Lenslet.app (macOS) or lenslet-ui + CLI (Linux)
#   --version X.Y.Z   LENSLET_VERSION      default: the latest release
#   --dir PATH        LENSLET_INSTALL_DIR  default: ~/.local/bin
#
# Linux gets a static CLI binary (any distro, any glibc); macOS a universal one.
# With --app, macOS installs Lenslet.app into /Applications (or ~/Applications),
# clears com.apple.quarantine, and links `lenslet` into --dir; Linux installs
# both `lenslet` and `lenslet-ui`. Every download is verified against SHA256SUMS.
set -eu

REPO="hxddh/lenslet"
VERSION="${LENSLET_VERSION:-}"
DIR="${LENSLET_INSTALL_DIR:-$HOME/.local/bin}"
INSTALL_APP="${LENSLET_INSTALL_APP:-0}"

while [ $# -gt 0 ]; do
  case "$1" in
    --app) INSTALL_APP="1"; shift ;;
    --version) VERSION="$2"; shift 2 ;;
    --dir) DIR="$2"; shift 2 ;;
    -h|--help) sed -n '2,15p' "$0" 2>/dev/null || true; exit 0 ;;
    *) echo "lenslet install: unknown option $1" >&2; exit 2 ;;
  esac
done

say() { echo "lenslet install: $*" >&2; }
fail() { say "$*"; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || fail "needs $1"; }
need curl
need uname

OS="$(uname -s)"
case "$OS" in
  Linux)
    need tar
    case "$(uname -m)" in
      x86_64|amd64) ARCH="x86_64"; TARGET="x86_64-linux-musl" ;;
      aarch64|arm64) ARCH="aarch64"; TARGET="aarch64-linux-musl" ;;
      *) fail "no build for Linux $(uname -m)" ;;
    esac ;;
  Darwin)
    ARCH="universal"
    TARGET="universal-macos" ;;
  *) fail "no build for $OS; see https://github.com/$REPO" ;;
esac

if [ -z "$VERSION" ]; then
  # The release page redirects to the newest tag, e.g. .../tag/lenslet-v11.0.0.
  URL=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")
  VERSION="${URL##*/lenslet-v}"
  case "$VERSION" in
    [0-9]*) ;;
    *) fail "could not find the latest version (got $URL); pass --version" ;;
  esac
fi
VERSION="${VERSION#v}"

# LENSLET_BASE_URL points at a mirror of the release assets (or a test server).
BASE="${LENSLET_BASE_URL:-https://github.com/$REPO/releases/download/lenslet-v$VERSION}"
TMP=$(mktemp -d)
cleanup() {
  if [ "$OS" = "Darwin" ] && [ -d "$TMP/mnt" ]; then
    hdiutil detach "$TMP/mnt" -quiet 2>/dev/null || true
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

verify_sha256() {
  asset="$1"
  expected=$(grep " $asset\$" "$TMP/SHA256SUMS" | cut -d' ' -f1)
  [ -n "$expected" ] || fail "$asset is not in SHA256SUMS"
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$TMP/$asset" | cut -d' ' -f1)
  else
    actual=$(shasum -a 256 "$TMP/$asset" | cut -d' ' -f1)
  fi
  [ "$expected" = "$actual" ] || fail "checksum mismatch for $asset"
}

if [ "$INSTALL_APP" = "1" ] && [ "$OS" = "Darwin" ]; then
  need hdiutil
  FILE="Lenslet-$VERSION.dmg"
  say "downloading Lenslet.app + CLI $VERSION (macOS universal)"
  curl -fsSL -o "$TMP/$FILE" "$BASE/$FILE" || fail "download failed: $BASE/$FILE"
  curl -fsSL -o "$TMP/SHA256SUMS" "$BASE/SHA256SUMS" || fail "download failed: $BASE/SHA256SUMS"
  verify_sha256 "$FILE"

  mkdir -p "$TMP/mnt"
  hdiutil attach -nobrowse -readonly -quiet -mountpoint "$TMP/mnt" "$TMP/$FILE" || fail "failed to mount $FILE"
  if [ -w "/Applications" ]; then
    APP_DIR="/Applications"
  else
    APP_DIR="$HOME/Applications"
    mkdir -p "$APP_DIR"
  fi
  APP_DEST="$APP_DIR/Lenslet.app"
  rm -rf "$APP_DEST"
  cp -R "$TMP/mnt/Lenslet.app" "$APP_DEST"
  xattr -cr "$APP_DEST" 2>/dev/null || true
  hdiutil detach "$TMP/mnt" -quiet 2>/dev/null || true

  mkdir -p "$DIR"
  ln -sf "$APP_DEST/Contents/MacOS/lenslet" "$DIR/lenslet"
  say "installed Lenslet.app to $APP_DEST"
  say "linked $("$DIR/lenslet" --version) to $DIR/lenslet"
elif [ "$INSTALL_APP" = "1" ] && [ "$OS" = "Linux" ]; then
  FILE="lenslet-$VERSION-$ARCH-linux.tar.gz"
  say "downloading Lenslet desktop + CLI $VERSION ($ARCH-linux)"
  curl -fsSL -o "$TMP/$FILE" "$BASE/$FILE" || fail "download failed: $BASE/$FILE"
  curl -fsSL -o "$TMP/SHA256SUMS" "$BASE/SHA256SUMS" || fail "download failed: $BASE/SHA256SUMS"
  verify_sha256 "$FILE"

  tar -xzf "$TMP/$FILE" -C "$TMP"
  EXTRACTED="$TMP/lenslet-$VERSION-$ARCH-linux"
  [ -d "$EXTRACTED" ] || EXTRACTED="$TMP"
  mkdir -p "$DIR"
  for bin in lenslet lenslet-ui; do
    install -m 0755 "$EXTRACTED/$bin" "$DIR/$bin" 2>/dev/null || {
      cp "$EXTRACTED/$bin" "$DIR/$bin" && chmod 0755 "$DIR/$bin"
    }
  done
  say "installed $("$DIR/lenslet" --version) and lenslet-ui to $DIR"
else
  need tar
  FILE="lenslet-cli-$VERSION-$TARGET.tar.gz"
  say "downloading lenslet $VERSION ($TARGET)"
  curl -fsSL -o "$TMP/$FILE" "$BASE/$FILE" || fail "download failed: $BASE/$FILE"
  curl -fsSL -o "$TMP/SHA256SUMS" "$BASE/SHA256SUMS" || fail "download failed: $BASE/SHA256SUMS"
  verify_sha256 "$FILE"

  tar -xzf "$TMP/$FILE" -C "$TMP"
  mkdir -p "$DIR"
  install -m 0755 "$TMP/lenslet" "$DIR/lenslet" 2>/dev/null || {
    cp "$TMP/lenslet" "$DIR/lenslet" && chmod 0755 "$DIR/lenslet"
  }
  say "installed $("$DIR/lenslet" --version) to $DIR/lenslet"
fi

case ":$PATH:" in
  *":$DIR:"*) ;;
  *) say "add $DIR to your PATH, e.g. export PATH=\"$DIR:\$PATH\"" ;;
esac
