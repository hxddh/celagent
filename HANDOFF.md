# HANDOFF — 项目交接文档(给下一个开发者/agent)

> 本文档是**唯一权威交接入口**。任何 agent 或开发者接手本项目,先从本文档开始。
> 配套文档:`README.md`(用户视角)、**`docs/architecture.md`(架构权威: 数据流/机制/设计决策/扩展点)**、
> `PACKAGING.md`(打包/发布)、`docs/distributed-deployment.md`(多机部署)、
> **`docs/project-evaluation.md`(2026-08-12 深度评估: 文档/代码对照、成熟度、优先改进)**。

## 0. 项目定位

**celagent**:独立开源 agent 产品。基于 Pi TUI 引擎 + Celld 分布式运行时 + BOS 对象存储,核心卖点是**会话永不丢(RPO=0)**——每轮对话经 BOS 权威落盘,崩溃/换机器/节点故障历史一条不丢。

**核心心智模型**:BOS 保数据(权威源,RPO=0)+ Celld 保执行(缓存/任务/集群)+ agent 可用 BOS(记忆工具)。

```
TUI 交互 (pi-coding-agent 引擎, 全量工具)
   │  turn_end 钩子 (不阻塞对话)
   ├─▶ worker SQLite (快速缓存, 2s 超时, 丢了可重建)
   └─▶ BOS 直写队列 (权威源, CAS If-Match 乐观锁 + 幂等去重)
                                │
                    sessions/<id>.json  (完整 content, 不截断)
```

**架构细节(数据流时序/机制原理/组件边界/10 项设计决策/扩展点)见 `docs/architecture.md`**——
改造架构或基于迭代前必读。

## 1. 代码地图

| 路径 | 职责 |
|------|------|
| `bin/celagent-tui.mjs` | CLI + TUI 主程序(~880 行:命令解析含 task 分布式任务、节点自动启动、turn_end 持久化钩子、会话恢复) |
| `src/bos.js` | BOS 直写核心(aws CLI 异步封装、CAS If-Match/If-None-Match、指数退避重试) |
| `src/bos-tools.js` | agent 内置记忆工具:`history_search`(跨会话检索)+ `session_snapshot`(显式快照),经 customTools 注入 pi 引擎 |
| `worker/src/index.js` | Celld worker(缓存读路径、Sync API、AWS SigV4 手写签名) |
| `worker/wrangler.jsonc` | worker 配置 |
| `install.sh` | 一键安装(正式模式:GitHub Release 下载平台二进制 + celld/worker;开发模式:CELAGENT_SRC) |
| `setup.sh` | 一键部署(凭证检测→建 bucket→部署 worker→启动双节点→写配置) |
| `scripts/node_mgr.sh` | 本机双节点管理(start/stop/status/restart,18090/18091) |
| `scripts/cluster_mgr.sh` | 多机集群管理(add-node/status 等) |
| `scripts/celld-bos-test.sh` | BOS 模式端到端测试 |
| `tests/core.test.mjs` | 核心回归(9 用例:需节点在跑;mock 模式 6 pass) |
| `tests/review-logic-proofs.test.mjs` | 评估缺陷源码锚定(9 pass,零依赖;修缺陷后应同步改本文件) |
| `tests/e2e-memory-tools.mjs` | 真实 LLM e2e(需 DEEPSEEK_API_KEY env) |
| `docs/celld-bos-architecture-demo.html` | 架构演示页(单文件、零依赖、33 轮真实对话实录回放, 2026-08-11) |
| `.github/workflows/ci.yml` | CI:syntax check + CLI smoke + 单元测试 + npm pack dry-run |

## 2. 开发环境与命令

### 快速上手(新环境,按序执行)

```bash
# 0. 前置: node >= 22 (pi 依赖链 undici 8.x 不支持 node 20)
node --version
# 1. 依赖 (pi 引擎等)
npm install
# 2. Celld 运行时 (测试/节点必需; 不在仓库内, 需单独安装)
curl -fsSL https://celld.dev/install.sh | sh    # 装到 ~/.local/bin/celld
# 3. BOS 凭证 (测试 BOS 链路必需)
aws configure --profile bos          # 配 AK/SK/region=bj
# 4. (仅首次) 一键部署: 建 bucket + 部署 worker + 写 settings.json + 启动双节点
#    ⚠️ CELAGENT_SRC 必须指向你的仓库路径 (setup.sh 默认找 ~/celagent)
CELAGENT_SRC=<仓库路径> ./setup.sh
# 5. 跑测试 (需节点在跑; 无节点时 mock 模式 6 pass)
node tests/core.test.mjs              # 9 用例全绿
```

### 常用命令

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
  - 或 `AWS_PROFILE=bos aws s3api list-buckets`(避坑指南: 统一 env 形式)
  - endpoint `https://s3.bj.bcebos.com`,凭证在 `~/.aws/credentials` 的 `[bos]` profile(动态读取)
- **LLM**:DeepSeek(OpenAI 兼容),key 在 `DEEPSEEK_API_KEY` 环境变量(无值时自动降级 mock)
- **凭证优先级**:完整环境变量 `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` 优先(必须成对);否则用 `AWS_PROFILE=bos`(不把 SK 读进脚本变量/Node 堆).两者不混用
- **install.sh 可配置 env**:`CELAGENT_REPO`(仓库地址)、`CELAGENT_SRC`(开发模式)、`CELAGENT_ROOT`(安装根目录,默认 ~/.local)、`CELAGENT_BUCKET`(强制 bucket)、`CELLD_ESBUILD`(esbuild 路径)
- **默认 bucket**:`celagent-<rand8>-<ts>`(随机后缀, 不含 whoami)
- **Celld**:不在仓库内,安装方式 `curl -fsSL https://celld.dev/install.sh | sh`(装到 `~/.local/bin/celld`);发布后随 Release 包分发
- **Pi 引擎**:npm 包 `@earendil-works/pi-coding-agent` v0.84.x(不 fork,库用)

## 3. 发布状态(v0.3.0 已发布; 当前阻塞见下)

### 已完成
- ✅ 代码功能:核心持久化(BOS 直写 + CAS + 幂等)、双节点、分布式部署、worker 缓存、记忆工具(history_search/session_snapshot)、完整记忆(BOS 写路径不截断 content)
- ✅ 仓库与认证: `github.com/hxddh/celagent` 已公开可推送
- ✅ **Release `v0.3.0` 已创建**(含 `celagent-{darwin-arm64,darwin-x64,linux-x64}`、`celld-darwin-arm64`、`install.sh`、`worker.tar.gz`)
- ✅ 版本统一为 `0.3.0`(`package.json` / `install.sh` / CLI)
- ✅ **安全净化(2026-08-12)**:当前树 + 可达 git 历史已 `filter-repo` 清除本机用户名/真实 session 指纹/AK 前缀打印;CI 含 Secret/PII 门禁;全部提交作者统一 `hxddh <hxddh@users.noreply.github.com>`;零密钥硬编码
  - ⚠️ GitHub 对 **已推送过的旧 SHA** 可能仍短期通过直接 commit URL 提供内容(平台保留孤儿对象);彻底从 github.com 抹掉需向 GitHub Support 申请 purge(仓库内 refs 已无锚点)
- ✅ 构建能力:Bun 单二进制编译通过并实测可运行(version/help/doctor/TUI);⚠️ 旧构建物已废弃
  (内含本机路径),发布构建必须按 PACKAGING.md 注意 0 在匿名路径/CI 执行
- ✅ README/install.sh 已指向 `github.com/hxddh/celagent/releases/latest/download/install.sh`
- ✅ **深度评估**:见 `docs/project-evaluation.md`(文档/代码对照、成熟度评分、P0–P3 改进清单)

### 当前阻塞(按优先级)
1. **数据正确性 P0**(评估 §12–§14):worker 覆盖权威;sync 抹新轮;队列丢最新;`ensureLock` 泄漏;**/fork 串写**;steer 不用 content;损坏 JSON 可覆盖全历史
2. **文档**:README/demo 仍 BOS-first 或「截断安全」;architecture 权威段本 PR 已改为目标/当前
3. **CI**:扫 `node_modules`;无 settings 挂死;`CELAGENT_MOCK` 未被读取;`npm test` 不含 proof
4. **合入 PR#1** + checksum/worker 鉴权等残留(§12.3)
5. **Release/安装**:缺 celld-linux;install 对 create-bucket/deploy **失败仍打印成功**
6. **doctor**:`models.json` 假阴性;Celld 离线不改变结论

### 发布后仍建议执行
0. ✅ **docs/archive 已删除**(2026-08-11 决策)
1. ✅ 建仓 + 推送 + Release v0.3.0
2. **修 CI 到全绿**(排除 node_modules 扫描;无配置时 CLI 用例必过;再考虑去掉 continue-on-error)
3. ✅ 版本已统一为 0.3.0
4. **补齐跨平台 celld 资产**(至少 `celld-linux-x64`;构建仍须匿名路径,见 PACKAGING.md 注意 0)
5. ✅ **install.sh 正式模式**已走 Release 下载
6. **修恢复读路径**(BOS-first 或 worker 存完整 body),并同步 `docs/architecture.md` §1.4
7. **端到端验证**:全新机器 `curl -fsSL https://github.com/hxddh/celagent/releases/latest/download/install.sh | sh`

## 4. 版本与里程碑

| 版本 | 内容 | 状态 |
|------|------|------|
| v0.1.x | CLI 骨架 + BOS 直写 + 双节点 | ✅ |
| v0.2.x | 分布式运行时(worker 缓存/sync、休眠唤醒、agent 任务化、cluster_mgr、多机部署文档) | ✅ |
| P1 记忆增强 | history_search + session_snapshot + 完整记忆(不截断) | ✅ 已并入 |
| v0.3.0 | **发布版**(含 P1 记忆增强):Release 二进制分发 + celld 随包 + install.sh 下载模式 | ✅ 已发 Release; 🔄 CI 全绿 / 多平台 celld / 读路径对齐 进行中 |

后续候选方向(未排期):多 provider 认证管理、快照浏览 UI、会话 diff/合并、Bucket 生命周期(降本)。

## 5. 工程约定(接手者必须遵守)

1. **中文交流**(代码注释/文档/commit message 用中文)
2. **先分析方案再动手**,不直接改;改动前说明影响面
3. **数据必须真实**——测试/演示数据来自实测,不得虚构(发布物中的演示数字需与实时 BOS 对齐)
4. **仓库安全红线**(发布前已多轮穷尽检查,任何新提交不得引入):
   - 不得提交任何 API key/凭证/密钥(含 git 历史)
   - 不得提交真实本机用户名、本机绝对路径(`/Users/...`、`/home/...`)、内部项目名(`celld-test`、`celagent-poc`)、真实 bucket/session id
   - **禁止在文档里写真实敏感样例当 denylist**(用抽象模式,如 `<local-user>` / `/Users/<user>/`)
   - 凭证一律运行时动态获取(优先 `AWS_PROFILE=bos`; 不全量把 SK 拷进脚本变量/子进程 env)
   - 默认 bucket 名不得含 `whoami`(用随机后缀)
   - 新增文件默认自检:`rg -n 'sk-[a-zA-Z0-9]{20,}|AKIA|ghp_|ALTAK|/Users/[A-Za-z0-9._-]+/' <文件>`
5. 提交作者统一 `hxddh <hxddh@users.noreply.github.com>`(新环境需自行 `git config user.name/email` 设置,开发机已设)
6. 改完跑测试:`node tests/core.test.mjs`(节点在跑时)+ 相关脚本 `bash -n` 语法检查 + CI secret/PII scan

## 6. 已知技术债/注意点

- `install.sh` 正式模式已走 Release 二进制下载;开发模式用 `CELAGENT_SRC`
- `install.sh` 中 celld: Release 随包优先, 回退 `https://celld.dev/install.sh`
- worker 缓存读路径有 200 字符截断(URL 限制所致),完整数据在 BOS 权威源
- HTML 演示页数字已按 2026-08-11 实时 BOS 对齐;回放为 33 轮真实会话实录(脱敏后),
  后续更新数据时保持与 BOS 一致 + 敏感扫描(见红线)
- ci.yml 的单元测试步骤带 `continue-on-error: true`(无节点时 mock 模式)——CI 全绿要求下应确认该步骤是否应改为必须通过
- CI 不构建发布二进制(当前跨平台构建为手动 `bun build`,见 PACKAGING)——后续可加 CI release job 自动化构建+上传
