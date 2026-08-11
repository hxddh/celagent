# HANDOFF — 项目交接文档(给下一个开发者/agent)

> 本文档是**唯一权威交接入口**。任何 agent 或开发者接手本项目,先从本文档开始。
> 配套文档:`README.md`(用户视角)、`PACKAGING.md`(打包/发布)、`docs/distributed-deployment.md`(多机部署)。

## 0. 项目定位

**celagent**:独立开源 agent 产品。基于 Pi TUI 引擎 + Celld 分布式运行时 + BOS 对象存储,核心卖点是**会话永不丢(RPO=0)**——每轮对话经 BOS 权威落盘,崩溃/换机器/节点故障历史一条不丢。

三层架构(BOS 保数据 + Celld 保执行 + agent 可用 BOS):

```
TUI 交互 (pi-coding-agent 引擎, 全量工具)
   │  turn_end 钩子 (不阻塞对话)
   ├─▶ worker SQLite (快速缓存, 2s 超时, 丢了可重建)
   └─▶ BOS 直写队列 (权威源, CAS If-Match 乐观锁 + 幂等去重)
                                │
                    sessions/<id>.json  (完整 content, 不截断)
```

关键设计决策:
- **BOS 直写不走 celld**——节点全挂时数据不丢(权威源是 BOS,不是 celld 状态)
- 分发 = **GitHub Release 二进制**(Bun 单文件,75MB,含 pi 全部依赖),celld 随包分发
- 凭证全部动态获取(env 或 `~/.aws/credentials` [bos] profile),仓库内零凭证

## 1. 代码地图

| 路径 | 职责 |
|------|------|
| `bin/celagent-tui.mjs` | CLI + TUI 主程序(~875 行:命令解析含 task 分布式任务、节点自动启动、turn_end 持久化钩子、会话恢复) |
| `src/bos.js` | BOS 直写核心(aws CLI 异步封装、CAS If-Match/If-None-Match、指数退避重试) |
| `src/bos-tools.js` | agent 内置记忆工具:`history_search`(跨会话检索)+ `session_snapshot`(显式快照),经 customTools 注入 pi 引擎 |
| `worker/src/index.js` | Celld worker(缓存读路径、Sync API、AWS SigV4 手写签名) |
| `worker/wrangler.jsonc` | worker 配置 |
| `install.sh` | 一键安装(正式模式:git clone + npm install;开发模式:CELAGENT_SRC) |
| `setup.sh` | 一键部署(凭证检测→建 bucket→部署 worker→启动双节点→写配置) |
| `scripts/node_mgr.sh` | 本机双节点管理(start/stop/status/restart,18090/18091) |
| `scripts/cluster_mgr.sh` | 多机集群管理(add-node/status 等) |
| `scripts/celld-bos-test.sh` | BOS 模式端到端测试 |
| `tests/core.test.mjs` | 核心回归(9 用例:需节点在跑;mock 模式 6 pass) |
| `tests/e2e-memory-tools.mjs` | 真实 LLM e2e(需 DEEPSEEK_API_KEY env) |
| `docs/celld-bos-architecture-demo.html` | 架构演示页(单文件、零依赖、60 轮真实数据回放) |
| `.github/workflows/ci.yml` | CI:syntax check + CLI smoke + 单元测试 + npm pack dry-run |

## 2. 开发环境与命令

```bash
# 测试 (需节点: 先 scripts/node_mgr.sh start)
node tests/core.test.mjs          # 9 用例: 需节点; 无节点时 mock 模式 6 pass

# 节点管理
./scripts/node_mgr.sh start|stop|status|restart

# 自检
node bin/celagent-tui.mjs doctor

# 真实 LLM e2e (需要 DEEPSEEK_API_KEY 环境变量)
node tests/e2e-memory-tools.mjs

# 分布式任务 (celld 状态机, 断点续跑)
node bin/celagent-tui.mjs task submit write-report 5
node bin/celagent-tui.mjs task status
node bin/celagent-tui.mjs task ledger
```

### 关键环境事实

- **BOS bucket**:开发用 bucket 名**不在本仓库写死**(安全红线)。查询方式:
  - `cat ~/.config/celagent/settings.json` → `persistence.bucket`
  - 或 `aws s3api list-buckets --profile bos`
  - endpoint `https://s3.bj.bcebos.com`,凭证在 `~/.aws/credentials` 的 `[bos]` profile(动态读取)
- **LLM**:DeepSeek(OpenAI 兼容),key 在 `DEEPSEEK_API_KEY` 环境变量(无值时自动降级 mock)
- **凭证优先级**:环境变量 `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` 优先(必须成对);否则用 `~/.aws/credentials` 的 `[bos]` profile(不混用)
- **install.sh 可配置 env**:`CELAGENT_REPO`(仓库地址)、`CELAGENT_SRC`(开发模式)、`CELAGENT_ROOT`(安装根目录,默认 ~/.local)、`CELAGENT_BUCKET`(强制 bucket)、`CELLD_ESBUILD`(esbuild 路径)
- **Celld**:开发机本机路径见本机安装位置;发布后随包分发到 `~/.local/bin`
- **Pi 引擎**:npm 包 `@earendil-works/pi-coding-agent` v0.84.x(不 fork,库用)

## 3. 发布状态(v0.3.0 发布,卡 GitHub 认证)

### 已完成
- ✅ 代码功能:核心持久化(BOS 直写 + CAS + 幂等)、双节点、分布式部署、worker 缓存、记忆工具(history_search/session_snapshot)、完整记忆(不截断 content)
- ✅ 测试:core 9 用例(节点在跑时全绿)、e2e 真实 LLM 验证
- ✅ **发布前安全检查(六轮穷尽)**:文件层 + git 历史层 + 对象库层 + 代码逻辑层全部干净;全部提交作者统一 `hxddh <hxddh@users.noreply.github.com>`;零密钥/零用户名/零本机路径/零真实 bucket 名
- ✅ 构建物:`celagent-bin`(75MB Bun 单二进制)mac arm64 已验证可运行(version/help/doctor/list/TUI 完整启动)
- ✅ README/install.sh 已指向 `github.com/hxddh/celagent/releases/latest/download/install.sh`

### 阻塞(唯一)
- **GitHub 认证未完成** → 无法建仓/推送/发 Release。两种方案:
  - **方案 A(推荐)**:`brew install gh` → `gh auth login`(选 GitHub.com → HTTPS → 浏览器 OAuth)→ `cd <本地仓库路径> && gh repo create celagent --public --source . --push`
  - **方案 B**:https://github.com/settings/tokens 创建 fine-grained PAT(仅授权 celagent repo)→ 网页建仓 → `git remote add origin https://github.com/hxddh/celagent.git` → push
- 认证完成后立即执行下方"发布流程"

### 发布流程(认证后按序执行)

0. ✅ **docs/archive 已删除**(2026-08-11 决策):POC 探索代码(src-legacy/poc-pi-sdk/
   p0-verify)及 75MB 旧二进制不再随公开仓库发布;有价值结论已在 HANDOFF/
   bos-compat 等文档中
1. **推送**:建仓 + push(gh repo create --push 或 git push -u origin main)
2. **CI 首跑**:`.github/workflows/ci.yml` 首次运行,修到全绿(node 20/22 矩阵)
3. **版本统一**:`install.sh` 的 `VERSION="0.1.0"` 与 `package.json` 的 `"0.2.0"` 不一致 → 统一为发布版本(建议 v0.3.0,因含 P1 记忆增强 + P2 分布式,已超 0.2.0 语义)
4. **跨平台构建**:
   ```bash
   bun build bin/celagent-tui.mjs --compile --outfile celagent-darwin-arm64
   bun build bin/celagent-tui.mjs --compile --target=bun-darwin-x64 --outfile celagent-darwin-x64
   bun build bin/celagent-tui.mjs --compile --target=bun-linux-x64 --outfile celagent-linux-x64
   ```
5. **install.sh 正式模式端到端**:install.sh 当前正式模式走 `git clone + npm install`(仓库安装),**尚未改成"curl GitHub Release 下载二进制"**——这是发布前最后一个改造点。目标:Release 资产含 `celagent-<平台>` + `celld` 二进制,install.sh 检测平台 → 下载对应二进制 → 装到 `~/.local/bin`
6. **创建 Release**:`gh release create v0.3.0` → 上传二进制 + celld + install.sh
7. **端到端验证**:全新机器 `curl -fsSL .../install.sh | sh` 真实走一遍

## 4. 版本与里程碑

| 版本 | 内容 | 状态 |
|------|------|------|
| v0.1.x | CLI 骨架 + BOS 直写 + 双节点 | ✅ |
| v0.2.x | 分布式运行时(worker 缓存/sync、休眠唤醒、agent 任务化、cluster_mgr、多机部署文档) | ✅ |
| P1 记忆增强 | history_search + session_snapshot + 完整记忆(不截断) | ✅ 已并入 |
| v0.3.0 | **发布版**(含 P1 记忆增强):Release 二进制分发 + celld 随包 + install.sh 下载模式 + CI 全绿 | 🔄 进行中(卡认证) |

后续候选方向(未排期):多 provider 认证管理、快照浏览 UI、会话 diff/合并、Bucket 生命周期(降本)。

## 5. 工程约定(接手者必须遵守)

1. **中文交流**(代码注释/文档/commit message 用中文)
2. **先分析方案再动手**,不直接改;改动前说明影响面
3. **数据必须真实**——测试/演示数据来自实测,不得虚构(发布物中的演示数字需与实时 BOS 对齐)
4. **仓库安全红线**(发布过六轮安全检查,任何新提交不得引入):
   - 不得提交任何 API key/凭证/密钥(含 git 历史)
   - 不得提交真实用户名(<local-user>)、本机路径(/Users/、celld-test、celagent-poc)、真实 bucket 名
   - 凭证一律运行时动态获取(env / aws configure)
   - 新增文件默认自检:`grep -rnE 'sk-[a-zA-Z0-9]{20,}|AKIA|ghp_|ALTAK' <文件>`
5. 提交作者统一 `hxddh <hxddh@users.noreply.github.com>`(git config 已设)
6. 改完跑测试:`node tests/core.test.mjs`(节点在跑时)+ 相关脚本 `bash -n` 语法检查

## 6. 已知技术债/注意点

- `install.sh` 正式模式仍依赖 `git clone`(发布改造点,见发布流程步骤 5)
- `install.sh` 中 celld 下载走 `https://celld.dev/install.sh`(celld 官方);Release 随包分发后应改为从本仓库 Release 下载(随包分发决策)
- worker 缓存读路径有 200 字符截断(URL 限制所致),完整数据在 BOS 权威源
- HTML 演示页数字需与实时 BOS 对齐(演示数据来自 2026-08-10 实测)
- ci.yml 的单元测试步骤带 `continue-on-error: true`(无节点时 mock 模式)——CI 全绿要求下应确认该步骤是否应改为必须通过
- CI 不构建发布二进制(当前跨平台构建为手动 `bun build`,见 PACKAGING)——后续可加 CI release job 自动化构建+上传
