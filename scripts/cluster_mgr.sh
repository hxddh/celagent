#!/bin/bash
# celagent 多机集群管理 — P2: 分布式 agent 运行时
# 用法: cluster_mgr.sh start|stop|status|add-node <port> <advertise>
# 多机原理: 所有节点共享同一 BOS bucket (fleet/nodes/ 注册表 + cell 状态),
#           节点经 BOS 发现彼此, agent 会话可在任意节点访问
set -e
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
export AWS_PROFILE=bos AWS_REGION=bj

CELLD="${CELLD:-$HOME/.local/bin/celld}"
BUCKET=$(jq -r '.persistence.bucket // empty' "$HOME/.config/celagent/settings.json" 2>/dev/null)
[ -z "$BUCKET" ] && BUCKET=$(cat /tmp/celld_e2e_bucket 2>/dev/null)
EP="https://s3.bj.bcebos.com"
STATE_DIR="$HOME/.local/celagent/state"
mkdir -p "$STATE_DIR"

# 默认双节点 (18090/18091) — 与 node_mgr 一致
DEFAULT_PORTS="18090 18091"

start_node() {
  local port=$1 advertise=$2
  # 凭证卫生: AWS_PROFILE=bos, 不把 SK 读进变量/显式注入
  nohup env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u AWS_SESSION_TOKEN \
    CELLD_WATCH="$STATE_DIR/node$port" AWS_PROFILE=bos AWS_REGION=bj \
    CELAGENT_WORKER_TOKEN="${CELAGENT_WORKER_TOKEN:-$(jq -r '.worker.token // empty' "$HOME/.config/celagent/settings.json" 2>/dev/null)}" \
    "$CELLD" --bucket "s3://${BUCKET}" --endpoint "$EP" --region bj \
    --listen "127.0.0.1:${port}" --advertise "$advertise" \
    > "$STATE_DIR/node$port.log" 2>&1 &
  echo "  ✓ 节点 $port 启动 (pid $!, advertise=$advertise)"
}

wait_ready() {
  local port=$1
  for i in $(seq 1 20); do
    if curl -s -m 2 "http://127.0.0.1:${port}/__celld/health" 2>/dev/null | grep -q ok; then
      echo "  ✓ 节点 $port ready"
      return 0
    fi
    sleep 1
  done
  echo "  ✗ 节点 $port 未就绪"
  return 1
}

case "${1:-status}" in
  start)
    pkill -f 'celld.*1809' 2>/dev/null || true
    sleep 2
    echo "启动双节点 (本地集群)..."
    for p in $DEFAULT_PORTS; do
      start_node "$p" "127.0.0.1:$p"
    done
    for p in $DEFAULT_PORTS; do wait_ready "$p" || true; done
    echo "集群就绪: http://127.0.0.1:18090 + :18091 (共享 bucket $BUCKET)"
    ;;
  add-node)
    # 多机: 在另一台机器执行 cluster_mgr.sh add-node <port> <advertise>
    # 例: cluster_mgr.sh add-node 19000 192.168.1.50:19000
    PORT="${2:-19000}"
    ADVERTISE="${3:-127.0.0.1:$PORT}"
    start_node "$PORT" "$ADVERTISE"
    wait_ready "$PORT" || true
    echo "新节点加入集群 (advertise=$ADVERTISE) — 与现有节点共享 $BUCKET"
    ;;
  status)
    echo "节点注册表 (BOS nodes/):"
    aws s3api list-objects-v2 --bucket "$BUCKET" --prefix "nodes/" \
      --endpoint-url "$EP" --query "Contents[].Key" --output json 2>/dev/null | \
      python3 -c "import json,sys; ks=json.load(sys.stdin); print(f'  {len(ks)} 个节点'); [print(f'  - {k.split(chr(95))[1][:12]}...') for k in ks]" 2>/dev/null || echo "  查询失败"
    echo "本地节点:"
    for p in 18090 18091 19000; do
      if curl -s -m 1 "http://127.0.0.1:${p}/__celld/health" 2>/dev/null | grep -q ok; then
        echo "  ✓ $p 运行中"
      fi
    done
    ;;
  stop)
    pkill -f 'celld.*1809' 2>/dev/null || true
    pkill -f 'celld.*19000' 2>/dev/null || true
    echo "已停止"
    ;;
  *)
    echo "用法: cluster_mgr.sh start|stop|status|add-node <port> <advertise>"
    echo "多机部署: 每台机器装 celld + 相同 settings.json (同 bucket),"
    echo "  然后 cluster_mgr.sh add-node <port> <本机IP:port> — 节点经 BOS 自动发现"
    ;;
esac
