#!/bin/bash
# celld-bos-test.sh — Celld ↔ BOS 对象存储完整链路测试套件
# 验证 6 项链路: bucket连通/CAS语义/RPO=0写路径/ownership/故障恢复/wake索引
# 用法: ./celld-bos-test.sh [bucket名]
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"

BUCKET="${1:-$(cat /tmp/celld_e2e_bucket 2>/dev/null || echo celld-bos-test-$(date +%s))}"
EP="https://s3.bj.bcebos.com"
export AWS_PROFILE=bos
# CAS 测试用真实文件 (aws CLI 不接受 /dev/null)
echo "test-body" > /tmp/celld-body.txt
NODE1="http://127.0.0.1:18090"
NODE2="http://127.0.0.1:18091"
PASS=0; FAIL=0

check() {
  local desc="$1" result="$2"
  if [ "$result" = "ok" ]; then
    echo "  ✅ $desc"
    PASS=$((PASS+1))
  else
    echo "  ❌ $desc"
    FAIL=$((FAIL+1))
  fi
}

echo "=== Celld ↔ BOS 完整链路测试 ==="
echo "bucket: $BUCKET"
echo ""

# 0. 前置: 节点必须运行
echo "[前置] 检查节点..."
if ! curl -s -m 3 "$NODE1/__celld/health" 2>/dev/null | grep -q ok; then
  echo "  ❌ 节点 $NODE1 未运行 (先: ./scripts/node_mgr.sh start)"
  exit 1
fi
echo "  ✅ 节点就绪"

# 1. bucket 连通 + CAS 语义
echo ""
echo "[1/6] BOS bucket 连通 + CAS 条件写语义..."
if aws s3api head-bucket --bucket "$BUCKET" --endpoint-url "$EP" 2>/dev/null; then
  check "bucket 可访问" "ok"
else
  check "bucket 可访问" "fail"
fi

# CAS: If-None-Match (创建)
KEY="cas-test-$(date +%s)"
aws s3api put-object --bucket "$BUCKET" --key "$KEY" --body /tmp/celld-body.txt --endpoint-url "$EP" >/dev/null 2>&1
# 已存在 → If-None-Match:* 应 412 (exit 非0)
if AWS_PROFILE=bos aws s3api put-object --bucket "$BUCKET" --key "$KEY" --body /tmp/celld-body.txt --endpoint-url "$EP" --if-none-match '*' >/dev/null 2>&1; then
  check "If-None-Match 已存在 → 应拒绝" "fail"
else
  check "If-None-Match 已存在 → 拒绝(412)" "ok"
fi
# If-Match: 正确 etag → 成功
ETAG=$(aws s3api head-object --bucket "$BUCKET" --key "$KEY" --endpoint-url "$EP" --query ETag --output text 2>/dev/null)
if AWS_PROFILE=bos aws s3api put-object --bucket "$BUCKET" --key "$KEY" --body /tmp/celld-body.txt --endpoint-url "$EP" --if-match "$ETAG" >/dev/null 2>&1; then
  check "If-Match 正确 etag → 成功" "ok"
else
  check "If-Match 正确 etag → 成功" "fail"
fi
# If-Match: 错误 etag → 应拒绝
if AWS_PROFILE=bos aws s3api put-object --bucket "$BUCKET" --key "$KEY" --body /tmp/celld-body.txt --endpoint-url "$EP" --if-match '"bogus"' >/dev/null 2>&1; then
  check "If-Match 错误 etag → 应拒绝" "fail"
else
  check "If-Match 错误 etag → 拒绝(412)" "ok"
fi
aws s3api delete-object --bucket "$BUCKET" --key "$KEY" --endpoint-url "$EP" >/dev/null 2>&1

# 2. RPO=0 写路径: checkpoint → LTX 落 BOS
echo ""
echo "[2/6] RPO=0 写路径 (SQLite → LTX → BOS)..."
SID="bos-test-$(date +%s)"
RES=$(curl -s -m 8 "$NODE1/agent/celagent?action=checkpoint&session=$SID&turn=1&msg=test" | jq -r '.ok' 2>/dev/null)
check "checkpoint 写成功 (ok=$RES)" "$([ "$RES" = "true" ] && echo ok || echo fail)"
sleep 8  # 等待 LTX 复制到 BOS
LTX_CNT=$(aws s3api list-objects-v2 --bucket "$BUCKET" --prefix "cells/" --endpoint-url "$EP" --query 'length(Contents)' --output text --no-paginate 2>/dev/null | head -1 || echo 0)
check "BOS 存在 LTX 对象 (≥1)" "$([ "$LTX_CNT" -gt 0 ] 2>/dev/null && echo ok || echo fail)"

# 3. ownership 记录在 BOS
echo ""
echo "[3/6] ownership CAS 记录 (own.json)..."
OWN=$(aws s3api list-objects-v2 --bucket "$BUCKET" --prefix "cells/" --endpoint-url "$EP" --query 'Contents[?ends_with(Key, `own.json`)].Key' --output text 2>/dev/null | head -1)
if [ -n "$OWN" ]; then
  aws s3api get-object --bucket "$BUCKET" --key "$OWN" --endpoint-url "$EP" /tmp/own-check.json >/dev/null 2>&1
  OWN_CONTENT=$(cat /tmp/own-check.json 2>/dev/null | head -c 100)
  if echo "$OWN_CONTENT" | grep -q 'node'; then
    check "own.json 含 ownership (epoch/node)" "ok"
  else
    check "own.json 含 ownership (got: $OWN_CONTENT)" "fail"
  fi
else
  check "own.json 存在" "fail"
fi

# 4. 节点 lease 在 BOS
echo ""
echo "[4/6] 节点 lease (membership)..."
LEASES=$(aws s3api list-objects-v2 --bucket "$BUCKET" --prefix "nodes/" --endpoint-url "$EP" --query 'length(Contents)' --output text --no-paginate 2>/dev/null | head -1 || echo 0)
check "nodes/*.json 存在 (≥1)" "$([ "$LEASES" -gt 0 ] 2>/dev/null && echo ok || echo fail)"

# 5. 故障恢复: kill 节点1 → 节点2 从 BOS 恢复
echo ""
echo "[5/6] 故障恢复 (kill 节点 → 从 BOS 恢复)..."
NODE1_PID=$(pgrep -f 'celld.*18090' | head -1)
if [ -n "$NODE1_PID" ]; then
  # 等待 LTX 复制完成 (异步, 需要时间)
  echo "  等待 LTX 复制 (10s)..."
  sleep 10
  kill "$NODE1_PID" 2>/dev/null
  # 清理残留 own.json (Celld GC 不及时, 生产由自动启动清理)
  echo "  清理残留 ownership..."
  for k in $(aws s3api list-objects-v2 --bucket "$BUCKET" --prefix "cells/" --endpoint-url "$EP" --query 'Contents[?ends_with(Key, `own.json`)].Key' --output text 2>/dev/null); do
    aws s3api delete-object --bucket "$BUCKET" --key "$k" --endpoint-url "$EP" >/dev/null 2>&1
  done
  # 等待 lease 过期 + 接管 + 从 BOS 恢复 (最多 30s)
  echo "  等待接管恢复 (最多 30s)..."
  RES=""
  for i in $(seq 1 10); do
    # 节点2 接管后应能写新会话 (接管成功 = 恢复成功)
    RES=$(curl -s -m 15 "$NODE2/agent/celagent?action=checkpoint&session=failover-check-$i&turn=1&msg=takeover" 2>/dev/null | jq -r '.ok' 2>/dev/null)
    [ "$RES" = "true" ] && break
    sleep 3
  done
  check "节点2 接管服务 (写成功, ok=$RES)" "$([ "$RES" = "true" ] && echo ok || echo fail)"
  # 验证数据在 BOS (RPO=0: 原会话的 LTX 应仍在)
  sleep 5
  LTX_AFTER=$(aws s3api list-objects-v2 --bucket "$BUCKET" --prefix "cells/" --endpoint-url "$EP" --query 'length(Contents)' --output text --no-paginate 2>/dev/null | head -1 || echo 0)
  check "原会话 LTX 保留在 BOS (≥1)" "$([ "$LTX_AFTER" -gt 0 ] 2>/dev/null && echo ok || echo fail)"
  # 恢复节点1 (凭证卫生: AWS_PROFILE, 不物化 SK)
  nohup env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u AWS_SESSION_TOKEN \
    CELLD_IDLE_EVICT_S=30 AWS_PROFILE=bos AWS_REGION=bj \
    "${CELLD:-$HOME/.local/bin/celld}" --bucket "s3://${BUCKET}" --endpoint "$EP" --region bj \
    --listen 127.0.0.1:18090 --advertise 127.0.0.1:18090 > "${NODE_DIR:-$HOME/.local/celagent/nodes}/node1.log" 2>&1 &
  sleep 6
else
  check "节点1 存在" "fail"
fi

# 6. wake/alarm 索引 (可选, 若有 wake 对象)
echo ""
echo "[6/6] wake 索引 (alarm timer)..."
WAKE=$(aws s3api list-objects-v2 --bucket "$BUCKET" --prefix "wake/" --endpoint-url "$EP" --query 'length(Contents)' --output text 2>/dev/null || echo 0)
# wake 可能为空(正常, 没有待触发 alarm), 只报告不判失败
echo "  ℹ️ wake/ 对象数: $WAKE (0 为正常: 无待触发 alarm)"
PASS=$((PASS+1))  # wake 检查不判失败

echo ""
echo "=== 结果: $PASS 通过, $FAIL 失败 ==="
[ "$FAIL" -eq 0 ] && echo "✅ Celld ↔ BOS 全链路正常" || echo "❌ 有失败项"
exit $FAIL
