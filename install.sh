#!/bin/sh
# Install the lenslet CLI.
#
#   curl -fsSL https://github.com/hxddh/lenslet/releases/latest/download/install.sh | sh
#
# Options (or environment variables):
#   --version X.Y.Z   LENSLET_VERSION      default: the latest release
#   --dir PATH        LENSLET_INSTALL_DIR  default: ~/.local/bin
#
# Linux gets a static binary (any distro, any glibc); macOS a universal one.
# The download is checked against the release's SHA256SUMS. The window is not
# installed here: on macOS use Lenslet.dmg, on Linux the full archive.
set -eu

REPO="hxddh/lenslet"
VERSION="${LENSLET_VERSION:-}"
DIR="${LENSLET_INSTALL_DIR:-$HOME/.local/bin}"

while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --dir) DIR="$2"; shift 2 ;;
    -h|--help) sed -n '2,13p' "$0" 2>/dev/null || true; exit 0 ;;
    *) echo "lenslet install: unknown option $1" >&2; exit 2 ;;
  esac
done

say() { echo "lenslet install: $*" >&2; }
fail() { say "$*"; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || fail "needs $1"; }
need curl
need tar
need uname

case "$(uname -s)" in
  Linux)
    case "$(uname -m)" in
      x86_64|amd64) TARGET="x86_64-linux-musl" ;;
      aarch64|arm64) TARGET="aarch64-linux-musl" ;;
      *) fail "no build for Linux $(uname -m)" ;;
    esac ;;
  Darwin) TARGET="universal-macos" ;;
  *) fail "no build for $(uname -s); see https://github.com/$REPO" ;;
esac

if [ -z "$VERSION" ]; then
  # The release page redirects to the newest tag, e.g. .../tag/lenslet-v4.0.0.
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
FILE="lenslet-cli-$VERSION-$TARGET.tar.gz"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM

say "downloading lenslet $VERSION ($TARGET)"
curl -fsSL -o "$TMP/$FILE" "$BASE/$FILE" || fail "download failed: $BASE/$FILE"
curl -fsSL -o "$TMP/SHA256SUMS" "$BASE/SHA256SUMS" || fail "download failed: $BASE/SHA256SUMS"

EXPECTED=$(grep " $FILE\$" "$TMP/SHA256SUMS" | cut -d' ' -f1)
[ -n "$EXPECTED" ] || fail "$FILE is not in SHA256SUMS"
if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL=$(sha256sum "$TMP/$FILE" | cut -d' ' -f1)
else
  ACTUAL=$(shasum -a 256 "$TMP/$FILE" | cut -d' ' -f1)
fi
[ "$EXPECTED" = "$ACTUAL" ] || fail "checksum mismatch for $FILE"

tar -xzf "$TMP/$FILE" -C "$TMP"
mkdir -p "$DIR"
install -m 0755 "$TMP/lenslet" "$DIR/lenslet" 2>/dev/null || {
  cp "$TMP/lenslet" "$DIR/lenslet" && chmod 0755 "$DIR/lenslet"
}
say "installed $("$DIR/lenslet" --version) to $DIR/lenslet"
case ":$PATH:" in
  *":$DIR:"*) ;;
  *) say "add $DIR to your PATH, e.g. export PATH=\"$DIR:\$PATH\"" ;;
esac
