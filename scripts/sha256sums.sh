#!/bin/bash
# 为 GitHub Release 资产生成 SHA256SUMS
# 用法: ./scripts/sha256sums.sh [含二进制的目录]
# 输出写到该目录的 SHA256SUMS; install.sh 正式模式会下载并 sha256sum -c
set -euo pipefail
DIR="${1:-.}"
cd "$DIR"

hash() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$@"
  else
    shasum -a 256 "$@"
  fi
}

shopt -s nullglob
files=()
for f in celagent-* celld-* install.sh worker.tar.gz; do
  [ -f "$f" ] || continue
  files+=("$f")
done
if [ "${#files[@]}" -eq 0 ]; then
  echo "✗ 目录内没有 Release 资产 (celagent-*/celld-*/install.sh/worker.tar.gz): $DIR" >&2
  exit 1
fi
hash "${files[@]}" | sort -k2 > SHA256SUMS
echo "✓ 已写 $DIR/SHA256SUMS (${#files[@]} 个文件)"
cat SHA256SUMS
