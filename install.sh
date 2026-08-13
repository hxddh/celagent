#!/bin/bash
# celagent v0.3.0 一键安装: CLI 二进制 + Celld 运行时 + BOS 对象存储持久化
# 正式模式: 从 GitHub Release 下载对应平台二进制 (含 celld 随包 + worker 源码包)
# 开发模式: CELAGENT_SRC=<源码目录> ./install.sh (软链直指源码, 改即生效)
set -e
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"

CELAGENT_ROOT="${CELAGENT_ROOT:-${HOME}/.local}"
VERSION="0.3.0"
REPO="https://github.com/hxddh/celagent"
RELEASE_URL="${CELAGENT_RELEASE_URL:-$REPO/releases/latest/download}"

# ---- 平台检测 (Release 资产命名: celagent-<平台>, celld-<平台>) ----
detect_platform() {
  local os arch
  os=$(uname -s)
  arch=$(uname -m)
  case "$os-$arch" in
    Darwin-arm64|Darwin-arm)  echo "darwin-arm64" ;;
    Darwin-x86_64|Darwin-x64) echo "darwin-x64" ;;
    Linux-x86_64|Linux-x64)   echo "linux-x64" ;;
    *) echo "unsupported" ;;
  esac
}
PLATFORM=$(detect_platform)
if [ "$PLATFORM" = "unsupported" ]; then
  echo "✗ 不支持的平台: $(uname -s)-$(uname -m) (支持 darwin-arm64/darwin-x64/linux-x64)"
  exit 1
fi

echo "=== celagent v${VERSION} 一键安装 (Celld + BOS, ${PLATFORM}) ==="

# 1. 前置检查
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
echo "[2/5] 安装 celagent CLI..."
mkdir -p "${CELAGENT_ROOT}/bin"
if [ -n "$CELAGENT_SRC" ]; then
  # 开发模式: 软链直指源码 (需含 bin/celagent-tui.mjs)
  if [ ! -f "${CELAGENT_SRC}/bin/celagent-tui.mjs" ]; then
    echo "  ✗ CELAGENT_SRC 下未找到 bin/celagent-tui.mjs: $CELAGENT_SRC"
    exit 1
  fi
  ln -sf "${CELAGENT_SRC}/bin/celagent-tui.mjs" "${CELAGENT_ROOT}/bin/celagent"
  echo "  ✓ celagent 已安装 (开发模式软链→源码)"
else
  # 正式模式: 从 GitHub Release 下载平台二进制
  echo "  下载 celagent-${PLATFORM} (${RELEASE_URL})..."
  curl -fsSL "${RELEASE_URL}/celagent-${PLATFORM}" -o "${CELAGENT_ROOT}/bin/celagent" || {
    echo "  ✗ 下载失败 (网络问题?) — 可设 CELAGENT_SRC 走开发模式, 或稍后重试"
    exit 1
  }
  chmod +x "${CELAGENT_ROOT}/bin/celagent"
  echo "  ✓ celagent 已安装 (${CELAGENT_ROOT}/bin/celagent, $(du -h "${CELAGENT_ROOT}/bin/celagent" | cut -f1))"
fi

# 3. 安装 celld 运行时 (Release 随包优先, 回退 celld.dev 官方)
echo "[3/5] 安装 celld 运行时..."
CELLD=""
for cand in "$HOME/.local/bin/celld" "${CELAGENT_ROOT}/bin/celld" "/usr/local/bin/celld"; do
  [ -x "$cand" ] && CELLD="$cand" && break
done
if [ -z "$CELLD" ]; then
  echo "  下载 celld-${PLATFORM} (随包分发)..."
  if curl -fsSL "${RELEASE_URL}/celld-${PLATFORM}" -o "${CELAGENT_ROOT}/bin/celld" 2>/dev/null; then
    chmod +x "${CELAGENT_ROOT}/bin/celld"
    CELLD="${CELAGENT_ROOT}/bin/celld"
    echo "  ✓ celld 随包安装 (${CELLD})"
  else
    echo "  Release 暂无 celld-${PLATFORM}, 回退 celld.dev 官方安装..."
    curl -fsSL https://celld.dev/install.sh | sh || {
      echo "  ✗ celld 下载失败 (网络问题?), 请手动安装"
      exit 1
    }
    CELLD="$HOME/.local/bin/celld"
  fi
fi
echo "  ✓ celld: $CELLD"

# 4. 检测 BOS 凭证 + 创建/复用 bucket (只验证存在, 不把 SK 读进 shell 变量)
echo "[4/5] 配置 BOS 对象存储..."
if ! aws configure get aws_access_key_id --profile bos >/dev/null 2>&1 \
  || ! aws configure get aws_secret_access_key --profile bos >/dev/null 2>&1; then
  echo "  ✗ 未找到 BOS 凭证 (需 ~/.aws/credentials 的 [bos] profile)"
  echo "  请配置: aws configure --profile bos"
  exit 1
fi
echo "  ✓ BOS 凭证可用"

# 默认名用随机后缀, 不含 whoami (避免 OS 用户名进入云资源名)
_rand() { openssl rand -hex 4 2>/dev/null || od -An -N4 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n'; }
EXISTING_BUCKET=$(jq -r '.persistence.bucket // empty' "$HOME/.config/celagent/settings.json" 2>/dev/null)
BUCKET="${CELAGENT_BUCKET:-${EXISTING_BUCKET:-celagent-$(_rand)-$(date +%s)}}"
if AWS_PROFILE=bos aws s3api head-bucket --bucket "$BUCKET" --endpoint-url "https://s3.bj.bcebos.com" 2>/dev/null; then
  echo "  ✓ bucket 已存在: $BUCKET"
else
  if ! AWS_PROFILE=bos aws s3api create-bucket --bucket "$BUCKET" --region bj --endpoint-url "https://s3.bj.bcebos.com"; then
    echo "  ✗ bucket 创建失败: $BUCKET"
    exit 1
  fi
  echo "  ✓ bucket 创建: $BUCKET"
fi

# 5. 部署 worker + 启动双节点 + 写配置
echo "[5/5] 部署 worker 并启动节点..."
# worker 源码: 开发模式用 CELAGENT_SRC/worker; 下载模式从 Release 取 worker.tar.gz
WORKER_SRC=""
if [ -n "$CELAGENT_SRC" ] && [ -d "${CELAGENT_SRC}/worker/src" ]; then
  WORKER_SRC="${CELAGENT_SRC}/worker"
else
  echo "  下载 worker 源码包..."
  mkdir -p "${CELAGENT_ROOT}/celagent"
  if curl -fsSL "${RELEASE_URL}/worker.tar.gz" -o /tmp/celagent-worker.tar.gz 2>/dev/null; then
    tar -xzf /tmp/celagent-worker.tar.gz -C "${CELAGENT_ROOT}/celagent/"
    WORKER_SRC="${CELAGENT_ROOT}/celagent/worker"
  fi
fi
if [ -n "$WORKER_SRC" ] && [ -d "$WORKER_SRC/src" ]; then
  # 凭证卫生: AWS_PROFILE, 不把 SK 注入进程环境
  export AWS_PROFILE=bos AWS_REGION=bj
  unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
  # esbuild: 优先本地 node_modules, 下载模式回退 npx (自动拉取)
  if [ -x "${CELAGENT_ROOT}/celagent/node_modules/.bin/esbuild" ]; then
    export CELLD_ESBUILD="${CELAGENT_ROOT}/celagent/node_modules/.bin/esbuild"
  elif [ -z "$CELLD_ESBUILD" ]; then
    export CELLD_ESBUILD="$(command -v npx >/dev/null 2>&1 && echo 'npx --yes esbuild' || echo '')"
  fi
  if (cd "$WORKER_SRC" && "$CELLD" deploy . --bucket "s3://${BUCKET}" --endpoint "https://s3.bj.bcebos.com" --region bj); then
    echo "  ✓ worker 已部署 (${WORKER_SRC})"
  else
    echo "  ✗ worker 部署失败"
    exit 1
  fi
else
  echo "  ⚠️ worker 源码不可用, 跳过部署 (节点可能以基础模式运行)"
fi

STATE_DIR="$HOME/.local/celagent/state"
mkdir -p "$STATE_DIR"
pkill -f 'celld.*1809' 2>/dev/null || true
sleep 2

for port in 18090 18091; do
  nohup env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u AWS_SESSION_TOKEN \
    CELLD_WATCH="$STATE_DIR/node$port" AWS_PROFILE=bos AWS_REGION=bj \
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
