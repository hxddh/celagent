# celagent 单二进制打包 (Bun)

## 已验证: Bun 编译 celagent 为自包含单二进制

- 二进制: `celagent-bin` (62MB, 含 Bun 运行时 + 全部依赖)
- 命令: `bun build bin/celagent.mjs --compile --outfile celagent-bin`
- 完整功能在二进制内工作:
  - 真实 LLM (DeepSeek) ✅
  - 工具调用 (get_time/calculate) ✅
  - checkpoint 到 Celld (RPO=0) ✅
  - 会话恢复 (含 toolCall/toolResult 完整角色) ✅

## 跨平台构建 (CI 中)

```bash
# macOS arm64 (本机)
bun build bin/celagent.mjs --compile --outfile celagent-darwin-arm64

# macOS x64
bun build bin/celagent.mjs --compile --target=bun-darwin-x64 --outfile celagent-darwin-x64

# Linux x64
bun build bin/celagent.mjs --compile --target=bun-linux-x64 --outfile celagent-linux-x64

# Windows
bun build bin/celagent.mjs --compile --target=bun-windows-x64 --outfile celagent-windows-x64.exe
```

CI matrix (GitHub Actions):
```yaml
strategy:
  matrix:
    include:
      - os: macos-14        # arm64
      - os: macos-13        # x64
      - os: ubuntu-latest   # linux-x64
```

## 分发形态

```
GitHub Release:
  celagent-darwin-arm64    (62MB 单文件)
  celagent-darwin-x64
  celagent-linux-x64
  celagent-windows-x64.exe
  install.sh               (curl|sh, 下载对应平台)

用户:
  curl -fsSL install.sh | sh
  → 下载 celagent 二进制 → ~/.local/bin
  → celagent local (首次自动下载 celld)
  → celagent chat
```

## 注意

1. 二进制含 DEEPSEEK_API_KEY 吗? 不含 — key 只在运行时从环境变量读
2. celld 运行时: 仍是独立下载 (install.sh 处理), 不打包进 celagent 二进制
3. 动态 import: Bun 编译支持 (已验证), 无需改动
