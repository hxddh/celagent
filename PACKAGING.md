# celagent 单二进制打包 (Bun)

## 状态

> **已验证 (2026-08-10)**: TUI 版 Bun 单二进制编译通过, 全功能运行正常。
> - 修复 Bug 84: 动态 import(file://路径) 无法被 Bun 打包 → 改为静态 import,
>   2984 modules 全部内联
> - 二进制 `celagent-bin` (75MB) 已重新生成并验证: version/help/doctor/list/TUI 完整启动 ✓

## 验证过的命令

- 二进制: `celagent-bin` (75MB, 含 Bun 运行时 + pi 全部依赖)
- 命令: `bun build bin/celagent-tui.mjs --compile --outfile celagent-bin`
- 已验证功能:
  - version/help/doctor/list ✅
  - TUI 完整启动 (services + 会话 + 渲染) ✅
  - 动态 import `../src/bos.js` ✅

## 跨平台构建

```bash
# macOS arm64 (本机)
bun build bin/celagent-tui.mjs --compile --outfile celagent-darwin-arm64

# macOS x64
bun build bin/celagent-tui.mjs --compile --target=bun-darwin-x64 --outfile celagent-darwin-x64

# Linux x64
bun build bin/celagent-tui.mjs --compile --target=bun-linux-x64 --outfile celagent-linux-x64

# Windows
bun build bin/celagent-tui.mjs --compile --target=bun-windows-x64 --outfile celagent-windows-x64.exe
```

## 分发形态

```
GitHub Release:
  celagent-darwin-arm64    (单文件)
  celagent-darwin-x64
  celagent-linux-x64
  celagent-windows-x64.exe
  install.sh               (curl|sh, 下载对应平台)

用户:
  curl -fsSL install.sh | sh
  → 下载 celagent → ~/.local/bin
  → celagent  (启动 TUI, 自动拉起 Celld 节点)
```

## 注意

0. **构建环境规范 (安全红线, 第八轮安全检查发现)**: Bun 编译会把模块绝对路径嵌入二进制
   (`__dirname`/模块注释)。**禁止在含用户名/项目名的路径下构建发布物** —
   旧构建实测含 `/Users/<local-user>/celagent-poc/...`。正确做法:
   ```bash
   rm -rf /tmp/anon-build && mkdir -p /tmp/anon-build
   cp -al bin src worker package.json package-lock.json scripts /tmp/anon-build/
   cp -al node_modules /tmp/anon-build/node_modules    # 必须硬链接复制, 不能符号链接
   cd /tmp/anon-build && bun build bin/celagent-tui.mjs --compile --outfile celagent-<平台>
   strings celagent-<平台> | grep -E "<local-user>|/Users/|celld-test"   # 必须为空
   ```
   或直接用 CI (GitHub Actions runner 路径 /home/runner/work/..., 天然干净)。
   仓库目录下的旧 `celagent-bin` (含本机路径) 已废弃, 严禁上传 Release。
1. 二进制不含 API key — 凭证运行时从环境变量 (AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY) 或 ~/.aws/credentials 的 [bos] profile 动态读取
2. Celld 运行时独立下载 (install.sh 处理), 不打包进 celagent 二进制
3. TUI 版动态 import: 需重新验证 Bun 编译 (打包前必做)
4. **发布前置**: 创建 GitHub 仓库 (默认 CELAGENT_REPO=https://github.com/hxddh/celagent.git),
   推送代码。当前 CI 仅 test/build (npm pack dry-run), 无 release 构建 job —
   二进制构建为手动 bun build (见上), 后续可加 CI release job 自动化
