# store_env.sh — 从 settings.json 读对象存储配置 (v0.3.3)
# 设置 STORE_BUCKET STORE_EP STORE_REGION STORE_PROFILE, 并 export AWS_PROFILE AWS_REGION
# 用法: . "$(dirname "$0")/store_env.sh"  或  . "$SCRIPT_DIR/scripts/store_env.sh"
# shellcheck shell=bash

STORE_EP_DEFAULT="https://s3.bj.bcebos.com"
STORE_PROFILE_DEFAULT="bos"

celagent_is_bcebos() {
  case "$1" in *bcebos.com*) return 0 ;; *) return 1 ;; esac
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
