#!/bin/bash
# celagent 节点管理脚本: 启动/停止/状态/重启 (产品化基础设施)
# 用法: node_mgr.sh start|stop|status|restart
set -e
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
export AWS_PROFILE=bos AWS_REGION=bj

CELLD="${CELLD:-$HOME/.local/bin/celld}"
# bucket 优先从 celagent 配置读 (与自动启动一致 — Bug 49), 回退旧测试值
BUCKET=$(jq -r '.persistence.bucket // empty' "$HOME/.config/celagent/settings.json" 2>/dev/null)
[ -z "$BUCKET" ] && BUCKET=$(cat /tmp/celld_e2e_bucket 2>/dev/null)
EP="https://s3.bj.bcebos.com"
WATCH1="${NODE_DIR:-$HOME/.local/celagent/nodes}/node1-watch"
WATCH2="${NODE_DIR:-$HOME/.local/celagent/nodes}/node2-watch"
LOG1="${NODE_DIR:-$HOME/.local/celagent/nodes}/node1.log"
LOG2="${NODE_DIR:-$HOME/.local/celagent/nodes}/node2.log"

start_node() {
  local port=$1 watch=$2 log=$3
  # Bug 93: 启动时截断旧日志 (celld 的 durability proof 刷屏会无限增长)
  # 保留最近 1MB, 避免日志无限膨胀
  if [ -f "$log" ]; then
    tail -c 1048576 "$log" > "$log.tmp" 2>/dev/null && mv "$log.tmp" "$log"
  fi
  # 凭证卫生: AWS_PROFILE=bos, 不把 SK 读进变量/显式注入
  nohup env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u AWS_SESSION_TOKEN \
    CELLD_WATCH="$watch" CELLD_IDLE_EVICT_S=30 AWS_PROFILE=bos AWS_REGION=bj \
    CELAGENT_WORKER_TOKEN="${CELAGENT_WORKER_TOKEN:-$(jq -r '.worker.token // empty' "$HOME/.config/celagent/settings.json" 2>/dev/null)}" \
    "$CELLD" --bucket "s3://${BUCKET}" --endpoint "$EP" --region bj \
    --listen "127.0.0.1:${port}" --advertise "127.0.0.1:${port}" > "$log" 2>&1 &
  echo "started node on $port (pid $!)"
}

# 等待节点就绪(最多 20s)
wait_ready() {
  local port=$1
  for i in $(seq 1 20); do
    if curl -s -m 2 "http://127.0.0.1:${port}/__celld/health" 2>/dev/null | grep -q ok; then
      echo "node $port ready"
      return 0
    fi
    sleep 1
  done
  echo "node $port NOT ready (tail log:)"
  tail -3 "$2" 2>/dev/null
  return 1
}

case "${1:-status}" in
  start)
    pkill -f 'celld.*1809' 2>/dev/null || true
    sleep 2
    # Bug 63: 先清理残留 own.json (与 ensureCelld 自动启动路径一致 — Bug 50 修复)
    # 旧节点被强杀后 own.json 指向死节点, 阻塞新节点接管 → RestoreFailed
    # (只有自动启动路径清理过, 手动重启路径遗漏 — 环境一致性缺陷)
    OWN_KEYS=$(aws s3api list-objects-v2 --bucket "$BUCKET" --endpoint-url "$EP" \
      --prefix "cells/" --query "Contents[?ends_with(Key, \`own.json\`)].Key" --output json 2>/dev/null || echo "[]")
    if [ "${CELAGENT_CLEAN_OWN:-}" = "1" ] || echo "$BUCKET" | grep -q '^celagent-'; then
      for k in $(echo "$OWN_KEYS" | jq -r '.[]?' 2>/dev/null); do
        aws s3api delete-object --bucket "$BUCKET" --key "$k" --endpoint-url "$EP" >/dev/null 2>&1
        echo "cleaned stale ownership: $k"
      done
    else
      echo "skip own.json wipe (bucket=$BUCKET 非 celagent- 前缀; CELAGENT_CLEAN_OWN=1 强制)"
    fi
    # BOS 预热(避免并发启动限流)
    aws s3api head-bucket --bucket "$BUCKET" --endpoint-url "$EP" >/dev/null 2>&1 || true
    sleep 2
    start_node 18090 "$WATCH1" "$LOG1"
    start_node 18091 "$WATCH2" "$LOG2"
    wait_ready 18090 "$LOG1" || true
    wait_ready 18091 "$LOG2" || true
    ;;
  stop)
    pkill -f 'celld.*1809' 2>/dev/null || true
    echo "stopped"
    ;;
  status)
    for p in 18090 18091; do
      if pgrep -f "celld.*${p}" >/dev/null; then
        echo "node $p: running"
      else
        echo "node $p: down"
      fi
    done
    ;;
  restart)
    "$0" stop
    sleep 2
    "$0" start
    ;;
  *)
    echo "用法: $0 start|stop|status|restart"
    ;;
esac
