#!/bin/bash
# celagent v0.3.6 一键安装: CLI 二进制 + Celld 运行时 + 对象存储持久化
# 正式模式: 从 GitHub Release 下载对应平台二进制 (含 celld 随包 + worker 源码包)
# 开发模式: CELAGENT_SRC=<源码目录> ./install.sh (软链直指源码, 改即生效)
set -e
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"

CELAGENT_ROOT="${CELAGENT_ROOT:-${HOME}/.local}"
VERSION="0.3.6"
REPO="https://github.com/hxddh/celagent"
RELEASE_URL="${CELAGENT_RELEASE_URL:-$REPO/releases/latest/download}"

# ---- 平台检测 (Release 资产命名: celagent-<平台>, celld-<平台>) ----
detect_platform() {
  local os arch
  os=$(uname -s)
  arch=$(uname -m)
  case "$os" in
    Darwin)
      case "$arch" in arm64|arm) echo "darwin-arm64" ;; x86_64|amd64) echo "darwin-x64" ;; *) echo "unsupported" ;; esac
      ;;
    Linux)
      case "$arch" in x86_64|amd64) echo "linux-x64" ;; aarch64|arm64) echo "linux-arm64" ;; *) echo "unsupported" ;; esac
      ;;
    MINGW*|MSYS*|CYGWIN*) echo "windows-x64" ;;
    *) echo "unsupported" ;;
  esac
}
PLATFORM=$(detect_platform)
if [ "$PLATFORM" = "unsupported" ]; then
  echo "✗ 不支持的平台: $(uname -s)-$(uname -m) (支持 darwin-arm64/darwin-x64/linux-x64/linux-arm64/windows-x64)"
  exit 1
fi
CLI_ASSET="celagent-${PLATFORM}"
[ "$PLATFORM" = "windows-x64" ] && CLI_ASSET="celagent-windows-x64.exe"

verify_checksums() {
  local dir="$1"
  local sums="$2"
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$dir" && sha256sum --ignore-missing -c "$sums")
  elif command -v shasum >/dev/null 2>&1; then
    (cd "$dir" && awk '{print $1"  "$2}' "$sums" | while read -r hash name; do
      [ -f "$name" ] || continue
      echo "$hash  $name"
    done | shasum -a 256 -c)
  else
    echo "  ⚠️ 无 sha256sum/shasum, 跳过校验"
    return 0
  fi
}

echo "=== celagent v${VERSION} 一键安装 (Celld + 对象存储, ${PLATFORM}) ==="

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
  echo "  下载 ${CLI_ASSET} (${RELEASE_URL})..."
  curl -fsSL "${RELEASE_URL}/${CLI_ASSET}" -o "${CELAGENT_ROOT}/bin/${CLI_ASSET}" || {
    echo "  ✗ 下载失败 (网络问题?) — 可设 CELAGENT_SRC 走开发模式, 或稍后重试"
    exit 1
  }
  chmod +x "${CELAGENT_ROOT}/bin/${CLI_ASSET}"
  ln -sfn "${CELAGENT_ROOT}/bin/${CLI_ASSET}" "${CELAGENT_ROOT}/bin/celagent"
  echo "  ✓ celagent 已安装 (${CELAGENT_ROOT}/bin/celagent, $(du -h "${CELAGENT_ROOT}/bin/${CLI_ASSET}" | cut -f1))"
  if curl -fsSL "${RELEASE_URL}/SHA256SUMS" -o "${CELAGENT_ROOT}/bin/.SHA256SUMS" \
    && [ -s "${CELAGENT_ROOT}/bin/.SHA256SUMS" ]; then
    if verify_checksums "${CELAGENT_ROOT}/bin" .SHA256SUMS; then
      echo "  ✓ 校验和通过"
    else
      echo "  ✗ 校验和失败"
      exit 1
    fi
  else
    echo "  ⚠️ Release 无 SHA256SUMS, 跳过校验 (设 CELAGENT_REQUIRE_CHECKSUM=1 则失败)"
    if [ "${CELAGENT_REQUIRE_CHECKSUM:-}" = "1" ]; then
      exit 1
    fi
  fi
fi

# 3. 安装 celld 运行时 (Release 随包优先, 回退 celld.dev 官方)
echo "[3/5] 安装 celld 运行时..."
CELLD=""
for cand in "$HOME/.local/bin/celld" "${CELAGENT_ROOT}/bin/celld" "/usr/local/bin/celld"; do
  [ -x "$cand" ] && CELLD="$cand" && break
done
if [ -z "$CELLD" ] && [ "$PLATFORM" = "windows-x64" ]; then
  echo "  ⚠️ 上游 celld 暂无 Windows 包, 跳过 celld (仅安装 celagent.exe)"
elif [ -z "$CELLD" ]; then
  echo "  下载 celld-${PLATFORM} (随包分发)..."
  if curl -fsSL "${RELEASE_URL}/celld-${PLATFORM}" -o "${CELAGENT_ROOT}/bin/celld-${PLATFORM}" 2>/dev/null \
    && [ -s "${CELAGENT_ROOT}/bin/celld-${PLATFORM}" ]; then
    chmod +x "${CELAGENT_ROOT}/bin/celld-${PLATFORM}"
    ln -sfn "${CELAGENT_ROOT}/bin/celld-${PLATFORM}" "${CELAGENT_ROOT}/bin/celld"
    CELLD="${CELAGENT_ROOT}/bin/celld"
    echo "  ✓ celld 随包安装 (${CELLD})"
    if [ -s "${CELAGENT_ROOT}/bin/.SHA256SUMS" ]; then
      verify_checksums "${CELAGENT_ROOT}/bin" .SHA256SUMS || { echo "  ✗ celld 校验和失败"; exit 1; }
    fi
  else
    rm -f "${CELAGENT_ROOT}/bin/celld-${PLATFORM}"
    echo "  Release 暂无 celld-${PLATFORM} (Intel Mac 上游未发), 回退 celld.dev 官方安装..."
    curl -fsSL https://celld.dev/install.sh | sh || {
      echo "  ✗ celld 下载失败 (网络问题?), 请手动安装"
      exit 1
    }
    CELLD="$HOME/.local/bin/celld"
  fi
fi
[ -n "$CELLD" ] && echo "  ✓ celld: $CELLD"

# 4. 检测凭证 + 创建/复用 bucket (只验证存在, 不把 SK 读进 shell 变量)
echo "[4/5] 配置对象存储..."
STORE_EP_DEFAULT="https://s3.bj.bcebos.com"
STORE_PROFILE_DEFAULT="bos"
SETTINGS_FILE="$HOME/.config/celagent/settings.json"
STORE_EP="$STORE_EP_DEFAULT"
STORE_PROFILE="$STORE_PROFILE_DEFAULT"
STORE_REGION=""
if [ -f "$SETTINGS_FILE" ]; then
  _ep=$(jq -r '.persistence.endpoint // empty' "$SETTINGS_FILE" 2>/dev/null || true)
  _pr=$(jq -r '.persistence.profile // empty' "$SETTINGS_FILE" 2>/dev/null || true)
  _rg=$(jq -r '.persistence.region // empty' "$SETTINGS_FILE" 2>/dev/null || true)
  [ -n "$_ep" ] && STORE_EP="$_ep"
  [ -n "$_pr" ] && STORE_PROFILE="$_pr"
  [ -n "$_rg" ] && STORE_REGION="$_rg"
fi
# endpoint 门禁: 规范实现在 scripts/store_env.sh (与 src/bos.js isAllowedEndpoint 对齐)。
# 开发模式 / 仓库内运行时直接 source; 仅 curl|bash 独立分发时用下面的内置回退
# (回退拷贝必须与 store_env.sh 保持逐字同步)
_EP_HELPER=""
for _cand in "${CELAGENT_SRC:+$CELAGENT_SRC/scripts/store_env.sh}" \
             "$(cd "$(dirname "$0")" 2>/dev/null && pwd)/scripts/store_env.sh"; do
  if [ -n "$_cand" ] && [ -f "$_cand" ]; then _EP_HELPER="$_cand"; break; fi
done
if [ -n "$_EP_HELPER" ]; then
  # shellcheck source=scripts/store_env.sh
  . "$_EP_HELPER"
  celagent_install_ep_ok() { celagent_is_allowed_endpoint "$1"; }
else
  celagent_install_ep_mid_ok() {
    case "$1" in ""|*.*|*[!a-z0-9-]*) return 1 ;; *) return 0 ;; esac
  }
  celagent_install_ep_ok() {
    local raw="${1%/}"
    [ -z "$raw" ] && return 1
    if [ "${CELAGENT_ALLOW_ENDPOINT:-}" = "1" ] || [ "${CELAGENT_ALLOW_ENDPOINT:-}" = "true" ]; then
      case "$raw" in http://*|https://*) return 0 ;; *) return 1 ;; esac
    fi
    local rest host scheme mid
    case "$raw" in
      http://*) scheme=http; rest="${raw#http://}" ;;
      https://*) scheme=https; rest="${raw#https://}" ;;
      *) return 1 ;;
    esac
    host="${rest%%/*}"
    case "$host" in
      \[*)
        # IPv6 字面量带方括号 ([::1] 或 [::1]:9000) — %%:* 会从第一个冒号截断
        host="${host%%]*}"; host="${host#\[}" ;;
      *) host="${host%%:*}" ;;
    esac
    host=$(printf '%s' "$host" | tr '[:upper:]' '[:lower:]')
    case "$host" in
      127.0.0.1|localhost|::1) return 0 ;;
    esac
    [ "$scheme" = https ] || return 1
    case "$host" in
      s3.bcebos.com|s3.amazonaws.com) return 0 ;;
      s3.*.bcebos.com)
        mid="${host#s3.}"; mid="${mid%.bcebos.com}"
        celagent_install_ep_mid_ok "$mid" && return 0 || return 1 ;;
      s3.*.amazonaws.com)
        mid="${host#s3.}"; mid="${mid%.amazonaws.com}"
        celagent_install_ep_mid_ok "$mid" && return 0 || return 1 ;;
      *.r2.cloudflarestorage.com) return 0 ;;
      fly.storage.tigris.dev|*.tigris.dev) return 0 ;;
      t3.storage.dev|*.t3.storage.dev) return 0 ;;
      *) return 1 ;;
    esac
  }
fi
if ! celagent_install_ep_ok "$STORE_EP"; then
  echo "  ✗ persistence.endpoint 不允许: $STORE_EP (仅 https 合格 host 或本机; 或设 CELAGENT_ALLOW_ENDPOINT=1)"
  exit 1
fi
if [ -z "$STORE_REGION" ]; then
  case "$STORE_EP" in
    *bcebos.com*) STORE_REGION=bj ;;
    *)
      echo "  ✗ 非 BOS endpoint 需要 persistence.region (celagent config set persistence.region <region>)"
      exit 1
      ;;
  esac
fi
export AWS_PROFILE="$STORE_PROFILE" AWS_REGION="$STORE_REGION"
HAS_ENV=0
if [ -n "${AWS_ACCESS_KEY_ID:-}" ] && [ -n "${AWS_SECRET_ACCESS_KEY:-}" ]; then HAS_ENV=1; fi
if [ "$HAS_ENV" != 1 ]; then
  if ! aws configure get aws_access_key_id --profile "$STORE_PROFILE" >/dev/null 2>&1 \
    || ! aws configure get aws_secret_access_key --profile "$STORE_PROFILE" >/dev/null 2>&1; then
    echo "  ✗ 未找到凭证 (需 ~/.aws/credentials 的 [$STORE_PROFILE] profile)"
    echo "  请配置: aws configure --profile $STORE_PROFILE"
    exit 1
  fi
fi
echo "  ✓ 凭证可用 (profile=$STORE_PROFILE endpoint=$STORE_EP)"

# 默认名用随机后缀, 不含 whoami (避免 OS 用户名进入云资源名)
_rand() { openssl rand -hex 4 2>/dev/null || od -An -N4 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n'; }
EXISTING_BUCKET=$(jq -r '.persistence.bucket // empty' "$HOME/.config/celagent/settings.json" 2>/dev/null)
BUCKET="${CELAGENT_BUCKET:-${EXISTING_BUCKET:-celagent-$(_rand)-$(date +%s)}}"
if AWS_PROFILE="$STORE_PROFILE" aws s3api head-bucket --bucket "$BUCKET" --endpoint-url "$STORE_EP" 2>/dev/null; then
  echo "  ✓ bucket 已存在: $BUCKET"
else
  case "$STORE_EP" in
    *bcebos.com*)
      if ! AWS_PROFILE="$STORE_PROFILE" aws s3api create-bucket --bucket "$BUCKET" --region "$STORE_REGION" --endpoint-url "$STORE_EP"; then
        echo "  ✗ bucket 创建失败: $BUCKET"
        exit 1
      fi
      echo "  ✓ bucket 创建: $BUCKET"
      ;;
    *)
      echo "  ✗ bucket 不存在。非 BOS 请先在控制台建桶: $BUCKET"
      exit 1
      ;;
  esac
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
  WORKER_TGZ=$(mktemp "${TMPDIR:-/tmp}/celagent-worker.XXXXXX.tar.gz")
  if curl -fsSL "${RELEASE_URL}/worker.tar.gz" -o "$WORKER_TGZ" 2>/dev/null; then
    if tar -tzf "$WORKER_TGZ" | grep -E '(^/)|(^\.\./)|(/\.\./)'; then
      echo "  ✗ worker.tar.gz 含非法路径, 拒绝解包"
      rm -f "$WORKER_TGZ"
    else
      tar -xzf "$WORKER_TGZ" -C "${CELAGENT_ROOT}/celagent/"
      WORKER_SRC="${CELAGENT_ROOT}/celagent/worker"
    fi
    rm -f "$WORKER_TGZ"
  fi
fi
if [ -n "$CELLD" ] && [ -n "$WORKER_SRC" ] && [ -d "$WORKER_SRC/src" ]; then
  # 凭证卫生: AWS_PROFILE, 不把 SK 注入进程环境
  export AWS_PROFILE="$STORE_PROFILE" AWS_REGION="$STORE_REGION"
  unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
  # esbuild: 优先本地 node_modules, 下载模式回退 npx (自动拉取)
  if [ -x "${CELAGENT_ROOT}/celagent/node_modules/.bin/esbuild" ]; then
    export CELLD_ESBUILD="${CELAGENT_ROOT}/celagent/node_modules/.bin/esbuild"
  elif [ -n "$CELLD_ESBUILD" ]; then
    :
  elif command -v esbuild >/dev/null 2>&1; then
    export CELLD_ESBUILD="$(command -v esbuild)"
  else
    echo "  ⚠️ 未找到 esbuild (设 CELLD_ESBUILD 或安装到 PATH), 跳过 npx 自动拉取"
    export CELLD_ESBUILD=""
  fi
  if (cd "$WORKER_SRC" && "$CELLD" deploy . --bucket "s3://${BUCKET}" --endpoint "$STORE_EP" --region "$STORE_REGION"); then
    echo "  ✓ worker 已部署 (${WORKER_SRC})"
  else
    echo "  ✗ worker 部署失败"
    exit 1
  fi
else
  echo "  ⚠️ worker 源码或 celld 不可用, 跳过部署 (节点可能以基础模式运行)"
fi

STATE_DIR="$HOME/.local/celagent/state"
mkdir -p "$STATE_DIR"
EXISTING_TOKEN=$(jq -r '.worker.token // empty' "$HOME/.config/celagent/settings.json" 2>/dev/null)
WORKER_TOKEN="${CELAGENT_WORKER_TOKEN:-${EXISTING_TOKEN:-$(_rand)$(_rand)}}"
if [ -z "$CELLD" ]; then
  echo "  ⚠️ 无 celld, 跳过启动节点"
else
  pkill -f 'celld.*1809' 2>/dev/null || true
  sleep 2
  for port in 18090 18091; do
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
fi

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
echo "  ✓ 配置已写入"

# CAS 探针 — 与 setup.sh 对齐: 条件写必须真正执行
# 退出码 2 = 探针未完成 (网络/凭证临时问题, 无法判定), 不能误报成存储不合格
echo "[CAS] 探针 (If-Match / If-None-Match / 写后读)..."
CAS_BIN=""
if [ -x "${CELAGENT_ROOT}/bin/celagent" ]; then
  CAS_BIN="${CELAGENT_ROOT}/bin/celagent"
elif command -v celagent >/dev/null 2>&1; then
  CAS_BIN="celagent"
fi
if [ -n "$CAS_BIN" ]; then
  CAS_RC=0
  "$CAS_BIN" cas-probe --bucket "$BUCKET" || CAS_RC=$?
  if [ "$CAS_RC" -eq 2 ]; then
    echo "  ✗ CAS 探针未完成 (临时网络/凭证问题, 无法判定存储能力)。请稍后重试安装, 或运行: celagent doctor"
    exit 1
  elif [ "$CAS_RC" -ne 0 ]; then
    echo "  ✗ 此存储不能保证 RPO=0 (条件写未生效)。换合格后端或检查权限后再安装。"
    exit 1
  fi
else
  echo "  ⚠ 未找到 celagent,跳过 CAS 探针 (随后请运行: celagent doctor)"
fi

echo ""
echo "=== 安装完成 ==="
echo "  celagent: ${CELAGENT_ROOT}/bin/celagent"
echo "  bucket:   $BUCKET"
echo "  节点:     18090 + 18091 (对象存储持久化)"
echo ""
echo "  使用: celagent   (把 ${CELAGENT_ROOT}/bin 加入 PATH)"
