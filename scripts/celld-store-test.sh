#!/bin/bash
# celld-store-test.sh — 对象存储完整链路测试(配置来自 settings,不限于 BOS)
exec "$(cd "$(dirname "$0")" && pwd)/celld-bos-test.sh" "$@"
