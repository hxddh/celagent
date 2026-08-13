#!/bin/bash
# celagent 节点管理脚本: 启动/停止/状态/重启 (产品化基础设施)
# 用法: node_mgr.sh start|stop|status|restart
set -e
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=store_env.sh
. "$SCRIPT_DIR/store_env.sh"
celagent_load_store || exit 1

CELLD="${CELLD:-$HOME/.local/bin/celld}"
# bucket 优先从 celagent 配置读 (与自动启动一致 — Bug 49), 回退旧测试值
BUCKET="${STORE_BUCKET:-}"
[ -z "$BUCKET" ] && BUCKET=$(cat /tmp/celld_e2e_bucket 2>/dev/null)
EP="$STORE_EP"
WATCH1="${NODE_DIR:-$HOME/.local/celagent/nodes}/node1-watch"
WATCH2="${NODE_DIR:-$HOME/.local/celagent/nodes}/node2-watch"
LOG1="${NODE_DIR:-$HOME/.local/celagent/nodes}/node1.log"
LOG2="${NODE_DIR:-$HOME/.local/celagent/nodes}/node2.log"
TOKEN="${CELAGENT_WORKER_TOKEN:-$(jq -r '.worker.token // empty' "$HOME/.config/celagent/settings.json" 2>/dev/null)}"

start_node() {
  local port=$1 watch=$2 log=$3
  # Bug 93: 启动时截断旧日志 (celld 的 durability proof 刷屏会无限增长)
  # 保留最近 1MB, 避免日志无限膨胀
  if [ -f "$log" ]; then
    tail -c 1048576 "$log" > "$log.tmp" 2>/dev/null && mv "$log.tmp" "$log"
  fi
  # 凭证卫生: AWS_PROFILE, 不把 SK 读进变量/显式注入
  # CELLD_VAR_* 才能进 worker env (celld v0.2)
  nohup env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u AWS_SESSION_TOKEN \
    CELLD_WATCH="$watch" \
    CELLD_IDLE_EVICT_S=30 \
    CELLD_ALARM_RESIDENT_MS=60000 \
    CELLD_ADMISSION_WAIT_MS=2000 \
    CELLD_MAX_RESIDENT_CELLS=128 \
    AWS_PROFILE="$STORE_PROFILE" AWS_REGION="$STORE_REGION" \
    CELAGENT_WORKER_TOKEN="$TOKEN" \
    CELLD_VAR_CELAGENT_WORKER_TOKEN="$TOKEN" \
    "$CELLD" --bucket "s3://${BUCKET}" --endpoint "$EP" --region "$STORE_REGION" \
    --listen "127.0.0.1:${port}" \
    --internal-listen "127.0.0.1:$((port + 2))" \
    --advertise "127.0.0.1:$((port + 2))" > "$log" 2>&1 &
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

# v0.2: 内部口 preserve 同机重启, 再 SIGTERM; 最多等 20s drain
stop_all() {
  for p in 18090 18091; do
    curl -s -m 2 -X POST "http://127.0.0.1:$((p + 2))/shutdown?handoff=preserve" >/dev/null 2>&1 || true
  done
  pkill -TERM -f 'celld.*1809' 2>/dev/null || true
  for i in $(seq 1 20); do
    up=0
    for p in 18090 18091; do
      if curl -s -m 1 "http://127.0.0.1:${p}/__celld/health" 2>/dev/null | grep -q ok; then
        up=1
      fi
    done
    [ "$up" = 0 ] && return 0
    sleep 1
  done
  echo "warn: drain timeout (nodes still answering health)"
  return 1
}

case "${1:-status}" in
  start)
    stop_all || true
    # 优雅停机后不扫 own.json; 仅崩溃残留(health 仍在)或 CELAGENT_CLEAN_OWN=1
    STILL_UP=0
    curl -s -m 1 "http://127.0.0.1:18090/__celld/health" 2>/dev/null | grep -q ok && STILL_UP=1
    curl -s -m 1 "http://127.0.0.1:18091/__celld/health" 2>/dev/null | grep -q ok && STILL_UP=1
    if [ "${CELAGENT_CLEAN_OWN:-}" = "1" ] || { [ "$STILL_UP" = "1" ] && echo "$BUCKET" | grep -q '^celagent-'; }; then
      OWN_KEYS=$(aws s3api list-objects-v2 --bucket "$BUCKET" --endpoint-url "$EP" \
        --prefix "cells/" --query "Contents[?ends_with(Key, \`own.json\`)].Key" --output json 2>/dev/null || echo "[]")
      for k in $(echo "$OWN_KEYS" | jq -r '.[]?' 2>/dev/null); do
        aws s3api delete-object --bucket "$BUCKET" --key "$k" --endpoint-url "$EP" >/dev/null 2>&1
        echo "cleaned stale ownership: $k"
      done
    else
      echo "skip own.json wipe (graceful drain 或非 celagent- 桶; CELAGENT_CLEAN_OWN=1 强制)"
    fi
    aws s3api head-bucket --bucket "$BUCKET" --endpoint-url "$EP" >/dev/null 2>&1 || true
    start_node 18090 "$WATCH1" "$LOG1"
    start_node 18091 "$WATCH2" "$LOG2"
    wait_ready 18090 "$LOG1" || true
    wait_ready 18091 "$LOG2" || true
    ;;
  stop)
    stop_all || true
    echo "stopped"
    ;;
  status)
    for p in 18090 18091; do
      if curl -s -m 1 "http://127.0.0.1:${p}/__celld/health" 2>/dev/null | grep -q ok; then
        echo "node $p: running"
        ST=$(curl -s -m 1 "http://127.0.0.1:$((p + 2))/state" 2>/dev/null || true)
        [ -n "$ST" ] && echo "  $ST"
      else
        echo "node $p: down"
      fi
    done
    ;;
  restart)
    "$0" stop
    "$0" start
    ;;
  *)
    echo "用法: $0 start|stop|status|restart"
    ;;
esac
