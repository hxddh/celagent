#!/bin/bash
# celagent 一键部署: Celld + BOS 对象存储持久化
# 用法: ./setup.sh [bucket名]
# 功能: ① 检测 BOS 凭证 ② 创建 bucket ③ 启动 BOS 模式 Celld 节点 ④ 配置 celagent
set -e
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"

echo "=== celagent 一键部署 (Celld + BOS 对象存储) ==="

# Bug 90: jq 是 bucket 复用逻辑的依赖, 缺失时静默新建 bucket 覆盖配置
command -v jq >/dev/null 2>&1 || { echo "  ✗ 需要 jq (brew install jq)"; exit 1; }

# 1. 检测 BOS 凭证
echo "[1/4] 检测 BOS 凭证..."
AK=$(aws configure get aws_access_key_id --profile bos 2>/dev/null || true)
SK=$(aws configure get aws_secret_access_key --profile bos 2>/dev/null || true)
if [ -z "$AK" ] || [ -z "$SK" ]; then
  echo "  ✗ 未找到 BOS 凭证 (需 ~/.aws/credentials 的 [bos] profile)"
  echo "  请配置: aws configure --profile bos"
  echo "    AWS Access Key ID: <你的 AK>"
  echo "    AWS Secret Access Key: <你的 SK>"
  echo "    Region: bj"
  exit 1
fi
echo "  ✓ BOS 凭证可用"

# 2. 创建 bucket — Bug 66: 优先复用已有配置的 bucket, 重装不丢数据
EXISTING_BUCKET=$(jq -r '.persistence.bucket // empty' "$HOME/.config/celagent/settings.json" 2>/dev/null)
BUCKET="${1:-${EXISTING_BUCKET:-celagent-$(whoami)-$(date +%s)}}"
echo "[2/4] 创建 bucket: $BUCKET"
if aws s3api head-bucket --bucket "$BUCKET" --endpoint-url "https://s3.bj.bcebos.com" --profile bos 2>/dev/null; then
  echo "  ✓ bucket 已存在"
else
  aws s3api create-bucket --bucket "$BUCKET" --region bj --endpoint-url "https://s3.bj.bcebos.com" --profile bos 2>&1 | head -2
  echo "  ✓ bucket 创建成功"
fi

# 3. 启动 BOS 模式 Celld 节点 (18090/18091)
echo "[3/4] 启动 BOS 模式 Celld 节点..."
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
  export AWS_ACCESS_KEY_ID="$AK" AWS_SECRET_ACCESS_KEY="$SK" AWS_REGION=bj
  export CELLD_ESBUILD="${CELLD_ESBUILD:-$SRC_WORKER/node_modules/.bin/esbuild}"
  (cd "$SRC_WORKER/worker" && "$CELLD" deploy . --bucket "s3://${BUCKET}" --endpoint "https://s3.bj.bcebos.com" --region bj 2>&1 | tail -2)
  echo "  ✓ worker 已部署"
fi

STATE_DIR="$HOME/.local/celagent/state"
mkdir -p "$STATE_DIR"
pkill -f 'celld.*1809' 2>/dev/null || true
sleep 2

# Bug 94: 与 node_mgr.sh 一致 — 启动前清理残留 own.json (旧节点被强杀后
# own.json 指向死节点, 阻塞新节点接管 → RestoreFailed)
OWN_KEYS=$(aws s3api list-objects-v2 --bucket "$BUCKET" --endpoint-url "https://s3.bj.bcebos.com" \
  --prefix "cells/" --query "Contents[?ends_with(Key, \`own.json\`)].Key" --output json 2>/dev/null || echo "[]")
for k in $(echo "$OWN_KEYS" | jq -r '.[]?' 2>/dev/null); do
  aws s3api delete-object --bucket "$BUCKET" --key "$k" --endpoint-url "https://s3.bj.bcebos.com" >/dev/null 2>&1
  echo "cleaned stale ownership: $k"
done

# Bug 71: 移除本地 worker.js 打包/检查死代码 — BOS 模式节点从 bucket 的
# deploy/current.json 加载 worker (上面 celld deploy 已部署), 本地 worker.js
# 从未被引用; 原逻辑 esbuild 失败还会错误地 exit 1 中止已成功的部署。

for port in 18090 18091; do
  nohup env CELLD_WATCH="$STATE_DIR/node$port" \
    AWS_ACCESS_KEY_ID="$AK" AWS_SECRET_ACCESS_KEY="$SK" AWS_REGION=bj \
    "$CELLD" --bucket "s3://${BUCKET}" --endpoint "https://s3.bj.bcebos.com" --region bj \
    --listen "127.0.0.1:${port}" --advertise "127.0.0.1:${port}" \
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

# 4. 配置 celagent
echo "[4/4] 配置 celagent..."
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
echo "  ✓ 配置已写入 $CONFIG_DIR/settings.json"

echo ""
echo "=== 部署完成 ==="
echo "  bucket: $BUCKET"
echo "  节点: 18090 + 18091 (BOS 持久化)"
echo "  使用: celagent"
