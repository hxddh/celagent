#!/bin/bash
# 在匿名路径编译 celagent、拉取上游 celld、打包 worker、生成 SHA256SUMS
# 用法: ./scripts/prepare-release-assets.sh [输出目录]
# 环境: SKIP_BUN=1 只拉 celld + 打包(不编译); CELLD_UPSTREAM_TAG 覆盖上游 tag
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-"$ROOT/dist/release"}"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"

CELLD_UPSTREAM_TAG="${CELLD_UPSTREAM_TAG:-v0.2.0}"
CELLD_UPSTREAM_BASE="https://github.com/denoland/celld/releases/download/${CELLD_UPSTREAM_TAG}"

fetch_celld() {
  local remote="$1" localname="$2"
  echo "  下载 $remote → $localname"
  curl -fsSL "${CELLD_UPSTREAM_BASE}/${remote}" -o "$OUT/${remote}"
  gzip -dc "$OUT/${remote}" > "$OUT/${localname}"
  chmod +x "$OUT/${localname}"
  rm -f "$OUT/${remote}"
}

echo "=== 拉取上游 celld ${CELLD_UPSTREAM_TAG} (denoland/celld) ==="
# 上游目前无 x86_64-apple-darwin / Windows; Intel Mac 与 Windows 仍回退 celld.dev
fetch_celld "celld-x86_64-unknown-linux-gnu.gz" "celld-linux-x64"
fetch_celld "celld-aarch64-unknown-linux-gnu.gz" "celld-linux-arm64"
fetch_celld "celld-aarch64-apple-darwin.gz" "celld-darwin-arm64"

echo "=== 打包 worker.tar.gz ==="
tar -C "$ROOT" -czf "$OUT/worker.tar.gz" \
  --exclude 'worker/node_modules' \
  --exclude 'worker/.wrangler' \
  worker
if tar -tzf "$OUT/worker.tar.gz" | grep -E '(^/)|(^\.\./)|(/\.\./)'; then
  echo "✗ worker.tar.gz 含非法路径" >&2
  exit 1
fi
cp "$ROOT/install.sh" "$OUT/install.sh"
chmod +x "$OUT/install.sh"
cp "$ROOT/install.ps1" "$OUT/install.ps1"

if [ "${SKIP_BUN:-}" = "1" ]; then
  echo "SKIP_BUN=1, 跳过 bun 编译"
else
  command -v bun >/dev/null 2>&1 || { echo "✗ 需要 bun (https://bun.sh)" >&2; exit 1; }
  ANON="${ANON_BUILD:-/tmp/anon-build}"
  echo "=== 匿名路径编译 ($ANON) ==="
  rm -rf "$ANON"
  mkdir -p "$ANON"
  cp -a "$ROOT/bin" "$ROOT/src" "$ROOT/worker" "$ROOT/package.json" "$ROOT/package-lock.json" "$ANON/"
  if [ -d "$ROOT/node_modules" ]; then
    cp -a "$ROOT/node_modules" "$ANON/node_modules"
  else
    (cd "$ANON" && npm ci)
  fi
  (
    cd "$ANON"
    bun build bin/celagent-tui.mjs --compile --outfile "$OUT/celagent-linux-x64"
    bun build bin/celagent-tui.mjs --compile --target=bun-linux-arm64 --outfile "$OUT/celagent-linux-arm64"
    bun build bin/celagent-tui.mjs --compile --target=bun-darwin-x64 --outfile "$OUT/celagent-darwin-x64"
    bun build bin/celagent-tui.mjs --compile --target=bun-darwin-arm64 --outfile "$OUT/celagent-darwin-arm64"
    bun build bin/celagent-tui.mjs --compile --target=bun-windows-x64 --outfile "$OUT/celagent-windows-x64.exe"
  )
  if command -v strings >/dev/null 2>&1; then
    if strings "$OUT/celagent-linux-x64" | grep -E '/Users/|/home/[^/]+/(celagent|celld)|celld-test|celagent-poc' >/dev/null; then
      echo "✗ 二进制嵌入了本机敏感路径" >&2
      exit 1
    fi
  fi
fi

echo "=== SHA256SUMS ==="
"$ROOT/scripts/sha256sums.sh" "$OUT"
echo "✓ 资产目录: $OUT"
ls -lh "$OUT"
