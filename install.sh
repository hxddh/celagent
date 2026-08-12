#!/bin/bash
# celagent 一键安装: CLI + Celld 运行时 + BOS 对象存储持久化
# 用法: curl -fsSL https://github.com/hxddh/celagent/releases/latest/download/install.sh | sh
#   或: CELAGENT_SRC=~/celagent ./install.sh   (开发模式)
set -e
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"

CELAGENT_ROOT="${CELAGENT_ROOT:-${HOME}/.local}"
VERSION="0.3.0"
echo "=== celagent v${VERSION} 一键安装 (Celld + BOS) ==="

# 1. 前置检查 — Bug 90: jq 是 bucket 复用逻辑的依赖, 缺失时静默新建 bucket
#    覆盖 settings.json (数据丢失风险) — 必须显式检查
for cmd in node curl aws jq; do
  command -v $cmd >/dev/null 2>&1 || { echo "  需要 $cmd (brew install jq)"; exit 1; }
done
# undici 8.x 依赖链要求 node >= 22 (node 20 运行时 import 崩溃)
NODE_MAJOR=$(node -e "console.log(process.versions.node.split('.')[0])" 2>/dev/null)
if [ "${NODE_MAJOR:-0}" -lt 22 ]; then
  echo "  ✗ 需要 node >= 22 (当前 $NODE_MAJOR) — pi 引擎依赖链 undici 8.x 不支持 node 20"
  exit 1
fi
echo "  ✓ node $NODE_MAJOR/curl/aws/jq 就绪"

# 2. 安装 celagent CLI
# Bug 58/66: 正式模式支持从 git 仓库/npm 包安装, 不再强制要求 CELAGENT_SRC
echo "[2/5] 安装 celagent CLI..."
CELAGENT_SRC="${CELAGENT_SRC:-}"
CELAGENT_REPO="${CELAGENT_REPO:-https://github.com/hxddh/celagent.git}"
if [ -z "$CELAGENT_SRC" ]; then
  echo "  从仓库安装 (CELAGENT_REPO=$CELAGENT_REPO)..."
  TMP_SRC="$(mktemp -d)/celagent"
  git clone --depth 1 "$CELAGENT_REPO" "$TMP_SRC" 2>/dev/null || {
    echo "  ✗ git clone 失败, 请设置 CELAGENT_SRC 指向源码目录 (开发模式)"
    echo "  开发模式: CELAGENT_SRC=~/celagent ./install.sh"
    exit 1
  }
  CELAGENT_SRC="$TMP_SRC"
fi
mkdir -p "${CELAGENT_ROOT}/bin" "${CELAGENT_ROOT}/celagent"
# Bug 68: 先清理目标目录再拷贝 — cp -r 不删除旧文件, 残留旧版 bin 会误导用户
rm -rf "${CELAGENT_ROOT}/celagent/bin" "${CELAGENT_ROOT}/celagent/src"
# Bug 58: 必须连 src/ 一起拷贝 (celagent-tui.mjs 运行时 import ../src/bos.js)
cp -r "${CELAGENT_SRC}/bin" "${CELAGENT_SRC}/src" "${CELAGENT_ROOT}/celagent/"
# 依赖安装: TUI 运行时 import @earendil-works/pi-coding-agent (Bug 58: 之前只拷 bin,
# 安装后启动即崩 — 找不到 pi 包)
if [ ! -d "${CELAGENT_ROOT}/celagent/node_modules/@earendil-works/pi-coding-agent" ] || [ ! -x "${CELAGENT_ROOT}/celagent/node_modules/.bin/esbuild" ]; then
  # Bug 92: esbuild 缺失也必须重装 — 旧安装目录的 package.json 可能没有 esbuild,
  # 导致 worker 部署静默失败
  echo "  安装依赖 (npm install)..."
  cp "${CELAGENT_SRC}/package.json" "${CELAGENT_SRC}/package-lock.json" "${CELAGENT_ROOT}/celagent/" 2>/dev/null || true
  (cd "${CELAGENT_ROOT}/celagent" && npm install --no-audit --no-fund) || {
    echo "  ✗ npm install 失败 (网络问题?), 请手动在 ${CELAGENT_ROOT}/celagent 执行 npm install"
    exit 1
  }
fi
# 链接 celagent 命令 (Bug H: 用 TUI 版, 与默认一致)
# Bug 67/92: 开发模式 (CELAGENT_SRC 指向本机源码) 时软链直接指向源码 —
# 改源码即生效 (Bug 47 修复)。但 git clone 的临时目录 (mktemp/正式安装)
# 也有 .git, 不能判为开发模式 — 只有 CELAGENT_SRC 是用户显式指定的
# 源码目录 (含 src/ 且有 package.json 且不在 /tmp) 才走开发模式
DEV_SRC=""
if [ -f "${CELAGENT_SRC}/bin/celagent-tui.mjs" ] && [ -d "${CELAGENT_SRC}/.git" ] \
   && [ -d "${CELAGENT_SRC}/src" ] && [ -f "${CELAGENT_SRC}/package.json" ] \
   && [ "${CELAGENT_SRC}" != "${TMP_SRC:-}" ] && [[ "${CELAGENT_SRC}" != /tmp/* ]]; then
  DEV_SRC="$CELAGENT_SRC"
fi
if [ -n "$DEV_SRC" ]; then
  ln -sf "${DEV_SRC}/bin/celagent-tui.mjs" "${CELAGENT_ROOT}/bin/celagent"
  echo "  ✓ celagent 已安装 (开发模式软链→源码: ${DEV_SRC}/bin/celagent-tui.mjs)"
else
  ln -sf "${CELAGENT_ROOT}/celagent/bin/celagent-tui.mjs" "${CELAGENT_ROOT}/bin/celagent"
  echo "  ✓ celagent 已安装到 ${CELAGENT_ROOT}/bin/celagent"
fi
chmod +x "${CELAGENT_ROOT}/bin/celagent"

# 3. 安装 celld 运行时
echo "[3/5] 安装 celld 运行时..."
CELLD=""
for cand in "$HOME/.local/bin/celld" "/usr/local/bin/celld"; do
  [ -x "$cand" ] && CELLD="$cand" && break
done
if [ -z "$CELLD" ]; then
  echo "  下载 celld..."
  curl -fsSL https://celld.dev/install.sh | sh || {
    echo "  ✗ celld 下载失败 (网络问题?), 请手动安装"
    exit 1
  }
  CELLD="$HOME/.local/bin/celld"
fi
echo "  ✓ celld: $CELLD"

# 4. 检测 BOS 凭证 + 创建 bucket
echo "[4/5] 配置 BOS 对象存储..."
AK=$(aws configure get aws_access_key_id --profile bos 2>/dev/null || true)
SK=$(aws configure get aws_secret_access_key --profile bos 2>/dev/null || true)
if [ -z "$AK" ] || [ -z "$SK" ]; then
  echo "  ✗ 未找到 BOS 凭证 (需 ~/.aws/credentials 的 [bos] profile)"
  echo "  请配置: aws configure --profile bos"
  exit 1
fi
echo "  ✓ BOS 凭证可用"

# 创建/复用 bucket — Bug 66: 已有 settings.json 时优先复用其 bucket,
# 绝不在重装时新建 bucket 丢失用户数据
EXISTING_BUCKET=$(jq -r '.persistence.bucket // empty' "$HOME/.config/celagent/settings.json" 2>/dev/null)
BUCKET="${CELAGENT_BUCKET:-${EXISTING_BUCKET:-celagent-$(whoami)-$(date +%s)}}"
if AWS_PROFILE=bos aws s3api head-bucket --bucket "$BUCKET" --endpoint-url "https://s3.bj.bcebos.com" 2>/dev/null; then
  echo "  ✓ bucket 已存在: $BUCKET"
else
  AWS_PROFILE=bos aws s3api create-bucket --bucket "$BUCKET" --region bj --endpoint-url "https://s3.bj.bcebos.com" >/dev/null 2>&1
  echo "  ✓ bucket 创建: $BUCKET"
fi

# 部署 worker 到 bucket (BOS 模式节点需要 deploy/current.json)
# Bug 86: worker 源码已纳入仓库 (worker/src/index.js) — 不再依赖开发机特有的
# 正式模式 (git clone 安装) 也能部署
echo "  部署 worker..."
WORKER_SRC="${CELAGENT_SRC}/worker"
if [ -d "$WORKER_SRC/src" ]; then
  export AWS_ACCESS_KEY_ID="$AK" AWS_SECRET_ACCESS_KEY="$SK" AWS_REGION=bj
  # Bug 86: esbuild 随仓库安装 (devDependency)
  export CELLD_ESBUILD="${CELLD_ESBUILD:-$CELAGENT_ROOT/celagent/node_modules/.bin/esbuild}"
  (cd "$WORKER_SRC" && "$CELLD" deploy . --bucket "s3://${BUCKET}" --endpoint "https://s3.bj.bcebos.com" --region bj >/dev/null 2>&1)
  echo "  ✓ worker 已部署 (${WORKER_SRC})"
else
  echo "  ✗ 未找到 worker 源码 (${WORKER_SRC}/src)"
  exit 1
fi

# 5. 启动 BOS 模式双节点 + 写配置
echo "[5/5] 启动节点并配置..."
STATE_DIR="$HOME/.local/celagent/state"
mkdir -p "$STATE_DIR"
pkill -f 'celld.*1809' 2>/dev/null || true
sleep 2

for port in 18090 18091; do
  nohup env CELLD_WATCH="$STATE_DIR/node$port" \
    AWS_ACCESS_KEY_ID="$AK" AWS_SECRET_ACCESS_KEY="$SK" AWS_REGION=bj \
    "$CELLD" --bucket "s3://${BUCKET}" --endpoint "https://s3.bj.bcebos.com" --region bj \
    --listen "127.0.0.1:${port}" --advertise "127.0.0.1:${port}" \
    > "$STATE_DIR/node$port.log" 2>&1 &
done
echo "  ✓ 节点 18090/18091 启动"

for i in $(seq 1 20); do
  R1=$(curl -s -m 2 "http://127.0.0.1:18090/__celld/health" 2>/dev/null || echo "")
  R2=$(curl -s -m 2 "http://127.0.0.1:18091/__celld/health" 2>/dev/null || echo "")
  if echo "$R1" | grep -q ok && echo "$R2" | grep -q ok; then
    echo "  ✓ 双节点就绪"
    break
  fi
  sleep 1
done

# 写 celagent 配置
CONFIG_DIR="$HOME/.config/celagent"
mkdir -p "$CONFIG_DIR"
cat > "$CONFIG_DIR/settings.json" <<EOF
{
  "provider": "deepseek",
  "model": "deepseek-v4-flash",
  "persistence": {
    "bucket": "$BUCKET",
    "endpoint": "https://s3.bj.bcebos.com",
    "region": "bj"
  }
}
EOF
echo "  ✓ 配置已写入"

echo ""
echo "=== 安装完成 ==="
echo "  celagent: ${CELAGENT_ROOT}/bin/celagent"
echo "  bucket:   $BUCKET"
echo "  节点:     18090 + 18091 (BOS 持久化)"
echo ""
echo "  使用: celagent   (把 ${CELAGENT_ROOT}/bin 加入 PATH)"
