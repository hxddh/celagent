#!/bin/bash
# celagent 一键部署: Celld + 对象存储持久化
# 用法: ./setup.sh [bucket名]
# 功能: ① 检测凭证 ② 创建/复用 bucket ③ 启动 Celld 节点 ④ 配置 celagent
set -e
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
ROOT=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=scripts/store_env.sh
. "$ROOT/scripts/store_env.sh"

echo "=== celagent 一键部署 (Celld + 对象存储持久化) ==="

# Bug 90: jq 是 bucket 复用逻辑的依赖, 缺失时静默新建 bucket 覆盖配置
command -v jq >/dev/null 2>&1 || { echo "  ✗ 需要 jq (brew install jq)"; exit 1; }

celagent_load_store || exit 1

# 1. 检测凭证 (只验证存在, 不把 SK 读进 shell 变量)
echo "[1/4] 检测对象存储凭证..."
HAS_ENV=0
if [ -n "${AWS_ACCESS_KEY_ID:-}" ] && [ -n "${AWS_SECRET_ACCESS_KEY:-}" ]; then HAS_ENV=1; fi
if [ "$HAS_ENV" != 1 ]; then
  if ! aws configure get aws_access_key_id --profile "$STORE_PROFILE" >/dev/null 2>&1 \
    || ! aws configure get aws_secret_access_key --profile "$STORE_PROFILE" >/dev/null 2>&1; then
    echo "  ✗ 未找到凭证 (需 ~/.aws/credentials 的 [$STORE_PROFILE] profile, 或 AWS_ACCESS_KEY_ID/SK)"
    echo "  请配置: aws configure --profile $STORE_PROFILE"
    exit 1
  fi
fi
echo "  ✓ 凭证可用 (profile=$STORE_PROFILE endpoint=$STORE_EP region=$STORE_REGION)"

# 2. 创建 bucket — Bug 66: 优先复用已有配置的 bucket, 重装不丢数据
# 默认名用随机后缀, 不含 whoami (避免 OS 用户名进入云资源名)
_rand() { openssl rand -hex 4 2>/dev/null || od -An -N4 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n'; }
EXISTING_BUCKET=$(jq -r '.persistence.bucket // empty' "$HOME/.config/celagent/settings.json" 2>/dev/null)
BUCKET="${1:-${EXISTING_BUCKET:-celagent-$(_rand)-$(date +%s)}}"
echo "[2/4] 创建/复用 bucket: $BUCKET"
if AWS_PROFILE="$STORE_PROFILE" aws s3api head-bucket --bucket "$BUCKET" --endpoint-url "$STORE_EP" 2>/dev/null; then
  echo "  ✓ bucket 已存在"
else
  if celagent_is_bcebos "$STORE_EP"; then
    if ! AWS_PROFILE="$STORE_PROFILE" aws s3api create-bucket --bucket "$BUCKET" --region "$STORE_REGION" --endpoint-url "$STORE_EP"; then
      echo "  ✗ bucket 创建失败: $BUCKET"
      exit 1
    fi
    echo "  ✓ bucket 创建成功"
  else
    echo "  ✗ bucket 不存在。非 BOS 请先在控制台建桶后再跑 setup.sh: $BUCKET"
    exit 1
  fi
fi

# 3. 启动 BOS 模式 Celld 节点 (18090/18091)
echo "[3/4] 启动 Celld 节点..."
# 探测 celld (常见位置)
CELLD=""
for cand in "$HOME/.local/bin/celld" "/usr/local/bin/celld" "/opt/homebrew/bin/celld"; do
  [ -x "$cand" ] && CELLD="$cand" && break
done
if [ -z "$CELLD" ]; then
  echo "  ✗ 未找到 celld 二进制 (需要先安装: curl -fsSL https://celld.dev/install.sh | sh)"
  exit 1
fi
echo "  celld: $CELLD"

# 部署 worker 到 bucket (BOS 模式节点需要 deploy/current.json)
# Bug 86: worker 源码在仓库内 worker/ 目录
SRC_WORKER="${CELAGENT_SRC:-$HOME/celagent}"
if [ -d "$SRC_WORKER/worker/src" ]; then
  echo "  部署 worker 到 bucket..."
  # 凭证卫生: 用 AWS_PROFILE, 不把 SK 注入进程环境
  export AWS_PROFILE="$STORE_PROFILE" AWS_REGION="$STORE_REGION"
  unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
  export CELLD_ESBUILD="${CELLD_ESBUILD:-$SRC_WORKER/node_modules/.bin/esbuild}"
  if ! (cd "$SRC_WORKER/worker" && "$CELLD" deploy . --bucket "s3://${BUCKET}" --endpoint "$STORE_EP" --region "$STORE_REGION"); then
    echo "  ✗ worker 部署失败"
    exit 1
  fi
  echo "  ✓ worker 已部署"
fi

STATE_DIR="$HOME/.local/celagent/state"
mkdir -p "$STATE_DIR"
pkill -f 'celld.*1809' 2>/dev/null || true
sleep 2

# Bug 94: 与 node_mgr.sh 一致 — 启动前清理残留 own.json (旧节点被强杀后
# own.json 指向死节点, 阻塞新节点接管 → RestoreFailed)
OWN_KEYS=$(aws s3api list-objects-v2 --bucket "$BUCKET" --endpoint-url "$STORE_EP" \
  --prefix "cells/" --query "Contents[?ends_with(Key, \`own.json\`)].Key" --output json 2>/dev/null || echo "[]")
if [ "${CELAGENT_CLEAN_OWN:-}" = "1" ] || echo "$BUCKET" | grep -q '^celagent-'; then
  for k in $(echo "$OWN_KEYS" | jq -r '.[]?' 2>/dev/null); do
    aws s3api delete-object --bucket "$BUCKET" --key "$k" --endpoint-url "$STORE_EP" >/dev/null 2>&1
    echo "cleaned stale ownership: $k"
  done
else
  echo "skip own.json wipe (bucket=$BUCKET 非 celagent- 前缀; CELAGENT_CLEAN_OWN=1 强制)"
fi

# Bug 71: 移除本地 worker.js 打包/检查死代码 — BOS 模式节点从 bucket 的
# deploy/current.json 加载 worker (上面 celld deploy 已部署), 本地 worker.js
# 从未被引用; 原逻辑 esbuild 失败还会错误地 exit 1 中止已成功的部署。

EXISTING_TOKEN=$(jq -r '.worker.token // empty' "$HOME/.config/celagent/settings.json" 2>/dev/null)
WORKER_TOKEN="${CELAGENT_WORKER_TOKEN:-${EXISTING_TOKEN:-$(_rand)$(_rand)}}"

for port in 18090 18091; do
  # 凭证卫生: celld 走 AWS_PROFILE=bos, 清除可能残留的显式密钥 env
  nohup env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u AWS_SESSION_TOKEN \
    CELLD_WATCH="$STATE_DIR/node$port" CELLD_IDLE_EVICT_S=30 \
    CELLD_ALARM_RESIDENT_MS=60000 CELLD_ADMISSION_WAIT_MS=2000 CELLD_MAX_RESIDENT_CELLS=128 \
    AWS_PROFILE="$STORE_PROFILE" AWS_REGION="$STORE_REGION" \
    CELAGENT_WORKER_TOKEN="$WORKER_TOKEN" \
    CELLD_VAR_CELAGENT_WORKER_TOKEN="$WORKER_TOKEN" \
    "$CELLD" --bucket "s3://${BUCKET}" --endpoint "$STORE_EP" --region "$STORE_REGION" \
    --listen "127.0.0.1:${port}" \
    --internal-listen "127.0.0.1:$((port + 2))" \
    --advertise "127.0.0.1:$((port + 2))" \
    > "$STATE_DIR/node$port.log" 2>&1 &
  echo "  ✓ 节点 $port 启动 (pid $!)"
done

# 等待就绪
echo "  等待节点就绪..."
for i in $(seq 1 20); do
  R1=$(curl -s -m 2 "http://127.0.0.1:18090/__celld/health" 2>/dev/null || echo "")
  R2=$(curl -s -m 2 "http://127.0.0.1:18091/__celld/health" 2>/dev/null || echo "")
  if echo "$R1" | grep -q ok && echo "$R2" | grep -q ok; then
    echo "  ✓ 双节点就绪"
    break
  fi
  sleep 1
done

# 4. 配置 celagent (已有非默认 persistence 则保留 endpoint/region/profile)
echo "[4/4] 配置 celagent..."
CONFIG_DIR="$HOME/.config/celagent"
mkdir -p "$CONFIG_DIR"
SETTINGS="$CONFIG_DIR/settings.json"
if [ -f "$SETTINGS" ]; then
  TMP=$(mktemp)
  jq --arg b "$BUCKET" --arg tok "$WORKER_TOKEN" --arg ep "$STORE_EP" --arg rg "$STORE_REGION" '
    .provider = (.provider // "deepseek")
    | .model = (.model // "deepseek-v4-flash")
    | .persistence = ((.persistence // {}) + {bucket: $b})
    | .persistence.endpoint = (.persistence.endpoint // $ep)
    | .persistence.region = (.persistence.region // $rg)
    | .worker = ((.worker // {}) + {token: $tok})
  ' "$SETTINGS" > "$TMP" && mv "$TMP" "$SETTINGS"
else
  cat > "$SETTINGS" <<EOF
{
  "provider": "deepseek",
  "model": "deepseek-v4-flash",
  "persistence": {
    "bucket": "$BUCKET",
    "endpoint": "$STORE_EP",
    "region": "$STORE_REGION"
  },
  "worker": {
    "token": "$WORKER_TOKEN"
  }
}
EOF
fi
chmod 600 "$SETTINGS"
echo "  ✓ 配置已写入 $SETTINGS"

echo ""
echo "=== 部署完成 ==="
echo "  bucket: $BUCKET"
echo "  节点: 18090/18091 (Worker) + 18092/18093 (celld 内部监听)"
echo "  使用: celagent"
