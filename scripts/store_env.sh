# store_env.sh — 从 settings.json 读对象存储配置 (v0.3.3)
# 设置 STORE_BUCKET STORE_EP STORE_REGION STORE_PROFILE, 并 export AWS_PROFILE AWS_REGION
# 用法: . "$(dirname "$0")/store_env.sh"  或  . "$SCRIPT_DIR/scripts/store_env.sh"
# shellcheck shell=bash

STORE_EP_DEFAULT="https://s3.bj.bcebos.com"
STORE_PROFILE_DEFAULT="bos"

celagent_is_bcebos() {
  case "$1" in *bcebos.com*) return 0 ;; *) return 1 ;; esac
}

# 与 src/bos.js isAllowedEndpoint 对齐: 非法 URL 拒绝,不改写成 BOS
# (install.sh 独立分发时内置一份同名回退拷贝 celagent_install_ep_ok — 改这里须同步)

# s3.<X>.bcebos.com / s3.<X>.amazonaws.com 的中段与 JS 正则 [a-z0-9-]+ 对齐:
# 只允许单标签 — glob 的 * 会吞点号, 否则 s3.a.b.bcebos.com 这类多标签 host
# 在这里放行、在 JS 运行时被拒, 造成 install 通过 / cas-probe 失败的错位
celagent_ep_mid_label_ok() {
  case "$1" in ""|*.*|*[!a-z0-9-]*) return 1 ;; *) return 0 ;; esac
}

celagent_is_allowed_endpoint() {
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
      # IPv6 字面量带方括号 ([::1] 或 [::1]:9000) — %%:* 会从第一个冒号截断, 必须先按 ] 剥
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
      celagent_ep_mid_label_ok "$mid" && return 0 || return 1 ;;
    s3.*.amazonaws.com)
      mid="${host#s3.}"; mid="${mid%.amazonaws.com}"
      celagent_ep_mid_label_ok "$mid" && return 0 || return 1 ;;
    *.r2.cloudflarestorage.com) return 0 ;;
    fly.storage.tigris.dev|*.tigris.dev) return 0 ;;
    t3.storage.dev|*.t3.storage.dev) return 0 ;;
    *) return 1 ;;
  esac
}

celagent_load_store() {
  local settings="${CELAGENT_SETTINGS:-$HOME/.config/celagent/settings.json}"
  STORE_BUCKET=""
  STORE_EP="$STORE_EP_DEFAULT"
  STORE_PROFILE="$STORE_PROFILE_DEFAULT"
  STORE_REGION=""
  if [ -f "$settings" ] && command -v jq >/dev/null 2>&1; then
    STORE_BUCKET=$(jq -r '.persistence.bucket // empty' "$settings" 2>/dev/null || true)
    local ep pr rg
    ep=$(jq -r '.persistence.endpoint // empty' "$settings" 2>/dev/null || true)
    pr=$(jq -r '.persistence.profile // empty' "$settings" 2>/dev/null || true)
    rg=$(jq -r '.persistence.region // empty' "$settings" 2>/dev/null || true)
    [ -n "$ep" ] && [ "$ep" != "null" ] && STORE_EP="$ep"
    [ -n "$pr" ] && [ "$pr" != "null" ] && STORE_PROFILE="$pr"
    [ -n "$rg" ] && [ "$rg" != "null" ] && STORE_REGION="$rg"
  fi
  if ! celagent_is_allowed_endpoint "$STORE_EP"; then
    echo "✗ persistence.endpoint 不允许: $STORE_EP (仅 https 合格 host 或本机; 或设 CELAGENT_ALLOW_ENDPOINT=1)" >&2
    return 1
  fi
  if [ -z "$STORE_REGION" ]; then
    if celagent_is_bcebos "$STORE_EP"; then
      STORE_REGION=bj
    else
      echo "✗ 非 BOS endpoint 需要 persistence.region (celagent config set persistence.region <region>)" >&2
      return 1
    fi
  fi
  export AWS_PROFILE="$STORE_PROFILE" AWS_REGION="$STORE_REGION"
  return 0
}
