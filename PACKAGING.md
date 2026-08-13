# celagent 单二进制打包 (Bun)

## 状态

> **已验证 (2026-08-10)**: TUI 版 Bun 单二进制编译通过, 全功能运行正常。
> - 修复 Bug 84: 动态 import(file://路径) 无法被 Bun 打包 → 改为静态 import,
>   2985 modules 全部内联
> - ⚠️ 当时的构建物 `celagent-bin` 已废弃(内含本机绝对路径, 第八轮安全检查发现),
>   发布构建必须按下方「注意 0」在匿名路径或 CI 执行

## 验证过的命令

- 二进制: 单文件 (~72MB, 含 Bun 运行时 + pi 全部依赖);当时构建物 `celagent-bin` 已废弃
- 命令: `bun build bin/celagent-tui.mjs --compile --outfile <匿名路径>/celagent-<平台>`
  (⚠️ 必须在匿名路径构建, 见「注意 0」; 禁止在仓库目录直接构建)
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
  celagent-linux-arm64
  celagent-windows-x64.exe
  install.sh
  install.ps1
  celld-linux-x64 / celld-linux-arm64 / celld-darwin-arm64
  worker.tar.gz
  SHA256SUMS

用户:
  curl -fsSL install.sh | sh
  → 下载 celagent → ~/.local/bin
  → celagent  (启动 TUI, 自动拉起 Celld 节点)
```

## 注意

0. **构建环境规范 (安全红线, 安全检查发现)**: Bun 编译会把模块绝对路径嵌入二进制
   (`__dirname`/模块注释)。**禁止在含用户名/项目名的路径下构建发布物** —
   旧构建实测含 `/Users/<user>/<project>/...`(本机用户名+项目名)。正确做法:
   ```bash
   rm -rf /tmp/anon-build && mkdir -p /tmp/anon-build
   cp -al bin src worker package.json package-lock.json scripts /tmp/anon-build/
   cp -al node_modules /tmp/anon-build/node_modules    # 必须硬链接复制, 不能符号链接
   cd /tmp/anon-build && bun build bin/celagent-tui.mjs --compile --outfile celagent-<平台>
   strings celagent-<平台> | grep -E '/Users/|/home/[^/]+/|celld-test|celagent-poc'   # 必须为空
   ```
   或直接用 CI (GitHub Actions runner 路径 /home/runner/work/..., 天然干净)。
   仓库目录下的旧 `celagent-bin` (含本机路径) 已废弃, 严禁上传 Release。
1. 二进制不含 API key — 凭证运行时从环境变量 (AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY) 或 ~/.aws/credentials 的 [bos] profile 动态读取
2. Celld 运行时独立下载 (install.sh 处理), 不打包进 celagent 二进制
3. TUI 版动态 import: 需重新验证 Bun 编译 (打包前必做)
4. **发布前置**: 仓库已是 `https://github.com/hxddh/celagent.git`。CI 含 test/build,以及 **Release job**
   (`.github/workflows/release.yml`): tag `v*` 或 workflow_dispatch 时在 `/tmp/anon-build` bun 交叉编译,
   拉取 `denoland/celld` 官方 gzip,打包 `worker.tar.gz`,生成 `SHA256SUMS` 并 `gh release upload --clobber`。
   本地:`./scripts/prepare-release-assets.sh dist/release`
5. **上游 celld 平台**(denoland/celld v0.2.0): 有 linux-x64 / linux-arm64 / darwin-arm64。
   **无** darwin-x64 / Windows — `install.sh` 这两平台回退 celld.dev 或跳过。
   `install.sh` 正式模式下载 `SHA256SUMS` 并校验; 文件不存在则警告(可 `CELAGENT_REQUIRE_CHECKSUM=1` 强制失败)。
