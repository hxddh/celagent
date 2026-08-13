#!/bin/bash
# 无 BOS/celld 的发布冒烟: 下载 linux 资产 → SHA256 校验 → 跑 version/help
# 用法: ./scripts/release-smoke.sh [tag]
# 环境: CELAGENT_RELEASE_URL 覆盖下载前缀
set -euo pipefail
TAG="${1:-latest}"
if [ "$TAG" = "latest" ]; then
  BASE="${CELAGENT_RELEASE_URL:-https://github.com/hxddh/celagent/releases/latest/download}"
else
  BASE="${CELAGENT_RELEASE_URL:-https://github.com/hxddh/celagent/releases/download/${TAG}}"
fi
DIR="$(mktemp -d "${TMPDIR:-/tmp}/celagent-smoke.XXXXXX")"
cleanup() { rm -rf "$DIR"; }
trap cleanup EXIT
cd "$DIR"
echo "=== release-smoke ($BASE) → $DIR ==="
curl -fsSL "$BASE/SHA256SUMS" -o SHA256SUMS
curl -fsSL "$BASE/celagent-linux-x64" -o celagent-linux-x64
curl -fsSL "$BASE/install.sh" -o install.sh
chmod +x celagent-linux-x64 install.sh
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum --ignore-missing -c SHA256SUMS
elif command -v shasum >/dev/null 2>&1; then
  awk '{print $1"  "$2}' SHA256SUMS | while read -r hash name; do
    [ -f "$name" ] || continue
    echo "$hash  $name"
  done | shasum -a 256 -c
else
  echo "✗ 无 sha256sum/shasum" >&2
  exit 1
fi
VER="$(./celagent-linux-x64 version)"
echo "$VER" | grep -q "celagent v" || { echo "✗ version 输出异常: $VER" >&2; exit 1; }
./celagent-linux-x64 help | grep -q "celagent <id>" || { echo "✗ help 输出异常" >&2; exit 1; }
echo "✓ $VER"
echo "✓ SHA256 + version/help 通过 (未测 BOS/celld 一键安装)"
