# HANDOFF — celagent 3.0

> 唯一权威交接入口。先读本文，再读 `docs/architecture.md`（架构）与 `docs/v3.0.md`（3.0 实现合同/验证）。
> 用户视角见 `README.md`。2.0 合同在 `docs/v2.0.md`；1.0 在 `docs/v1.0.md`；旧 0.x 文档在 `docs/archive/`，不要按它们改当前代码。

## 0. 项目定位

**celagent 3.0 = durable delegation。**

- Pi 1.5 负责 agent 语义（LLM、工具、compaction、TUI）；
- tmux supervisor + watchdog 负责执行者：客户端 detach 后工作继续；崩溃后拉起；
- celagent 负责把每个 step 变成对象存储里的不可变对象；
- 3.0 让 agent 用 `delegate` 派生子 agent：每个子 agent 是完整 session，有独立 worktree/lease/journal/checkpoint；
- 父进程退出或崩溃，子 agent 继续跑；结果可再取回；
- 单写者 lease + epoch + 心跳，跨机可顺序接管；
- 本地 interval trigger preview；cron/webhook/审批在 3.1；
- celld 0.6.3 是可选后端，核心不依赖。

保证：

```text
客户端断开 / 终端关闭，supervisor 继续执行
工具结果后自动生成增量工作区检查点
崩溃后从最后一个检查点和 manifest 恢复
进行中的工具最多一个 unknown；没有幂等键的副作用绝不静默重放
子 agent 独立 worktree/session；父死子不死；turn 中间崩溃不重放
```

## 1. 代码地图

| 路径 | 职责 |
|---|---|
| `bin/celagent-tui.mjs` | CLI/TUI/headless/up/attach/down/ps/logs/trigger；v2 装配、checkpoint runner、usage |
| `src/v2/session.js` | 分段 JSONL：append、materialize、readSession、list、delete、importV0 |
| `src/v2/manifest.js` | manifest 校验 / 读 / CAS 写 |
| `src/v2/lease.js` | 单写者租约、epoch、同机死 pid 接管、心跳 |
| `src/v2/outbox.js` | 本地 fsync 发件箱；失败留到下次启动 |
| `src/v2/journal.js` | 工具 journal start/finish/reconcile |
| `src/v2/extension.js` | Pi 1.5 durability extension（tool_call block / tool_result / checkpoint 请求） |
| `src/v2/workspace.js` | 1.0 tar workspace（保留兼容/测试） |
| `src/v2/workspace-cas.js` | **2.0 内容寻址增量检查点**：scan/blob/checkpoint/restore/runner |
| `src/v2/supervisor.js` | **tmux supervisor/watchdog**：start/attach/stop/status |
| `src/v2/usage.js` | Pi usage → manifest stats |
| `src/v2/triggers.js` | **本地 interval trigger preview**：配置/到期/daemon |
| `src/v2/adapter.js` | 对象存储适配器（BOS/S3 经 aws CLI；内存 store 测试） |
| `src/v2/crypto.js` | 可选 AES-256-GCM 信封加密 |
| `src/v2/status.js` | durable seq / lease / outbox / workspace / journal / stats |
| `src/v3/delegate.js` | **3.0 委派**：spawn/wait/send/list；工具定义；并发上限 |
| `src/v3/child.js` | **子 agent 本地协议**：task/state/result/inbox/output.log；prompt 状态机 |
| `src/v3/worktree.js` | **worktree 隔离**：创建/提交/摘要/清理 |
| `src/bos.js` | aws CLI 原语：get/put/list/delete、CAS 探针、endpoint 门禁 |
| `src/bos-tools.js` | `history_search` / `session_snapshot`（v2） |
| `src/persist.js` | 旧格式工具函数（`jsonlFromTurns` 仍被 `import-v0` 用） |
| `worker/src/index.js` | celld 可选缓存 worker（无模拟任务） |
| `scripts/node_mgr.sh` | 本机 celld 节点管理（0.6.3 health 路径） |
| `scripts/cluster_mgr.sh` | 多机 celld 节点管理（可选） |
| `install.sh` / `setup.sh` | 安装 / 部署（celld 0.6.3；tmux 检查） |
| `tests/v2-checkpoint.test.mjs` | 增量检查点/blob 复用/symlink/force/runner |
| `tests/v3-child.test.mjs` | 子 agent 状态机（task/inbox/interrupted/finalize） |
| `tests/v3-worktree.test.mjs` | worktree 创建/提交/清理、脏父仓库 |
| `tests/v3-delegate.test.mjs` | spawn/wait/send/list、并发上限、非 git 回退 |
| `tests/v2-supervisor.test.mjs` | tmux supervisor 生命周期（含 child kind） |
| `tests/v2-triggers.test.mjs` | trigger 配置/到期/删除 |
| `tests/v2-usage.test.mjs` | usage → manifest.stats |
| `tests/contract.test.mjs` | 发布合同（版本/依赖/模块/CLI） |
| `tests/v2.test.mjs` | 1.0 分段/租约/outbox/journal/加密/import/旧 workspace |
| `tests/core.test.mjs` | CLI + 可选 celld/BOS |
| `tests/cas-probe.test.mjs` | CAS 探针语义 |
| `tests/p0-p2-bugs.test.mjs` | endpoint/store_env 回归 |

## 2. 开发与验证

```bash
node --version                 # >= 22.19
npm install
npm test                       # 无 celld/真实桶时相关用例 skip

# 本地 S3 兼容（moto）端到端
# 需要 aws CLI + moto；见 docs/v2.0.md §8
node bin/celagent-tui.mjs doctor
node bin/celagent-tui.mjs run --session demo --prompt "hello" --json
node bin/celagent-tui.mjs status demo
node bin/celagent-tui.mjs up demo --cwd /tmp/ws
node bin/celagent-tui.mjs attach demo
node bin/celagent-tui.mjs down demo

# 委派（需要 bucket + tmux + git 仓库）
node bin/celagent-tui.mjs delegate spawn "只修 tests/a.test.mjs" --cwd ~/repo
node bin/celagent-tui.mjs ps --tree
node bin/celagent-tui.mjs delegate wait <dlg-id>
node bin/celagent-tui.mjs logs <dlg-id>
```

环境事实：

- **BOS 默认**：`https://s3.bj.bcebos.com`，region `bj`，profile `bos`。
- **aws CLI 是硬依赖**：`install.sh` 会检查；缺失时 `doctor` 明确报错。
- **tmux 是 2.0 supervisor / 3.0 子 agent 依赖**：`up/attach/delegate` 需要；`run/list/workspace` 不需要。
- **git 是 3.0 委派依赖**：子 agent 默认从 `HEAD` 建 worktree；非 git 目录只能 `isolate:false`。
- **Pi 精确 1.5.2**：不要升 caret；升级要跑 TUI + headless + journal + checkpoint 冒烟。
- **celld 0.6.3**：health 是 `/.well-known/celld/health`；不要按 bucket 前缀删 `own.json`。
- **加密**：`CELAGENT_ENCRYPTION_KEY` 32 字节 base64；整桶模式；key 丢失不可读。
- **workspace**：增量内容寻址；默认排除 `.git`/`node_modules`/`.venv`/`dist`/`build`/`.cache`/`target`/`vendor`；单文件 128 MB、总量 1 GB。

## 3. 发布状态

- 版本：**3.0.0**
- Latest：[v3.0.0](https://github.com/hxddh/celagent/releases/tag/v3.0.0)（待发布后回填时间）
- tag：`v3.0.0` → `TBD`（main）
- 依赖：pi `1.5.2`（exact）；celld `0.6.3`（随包）；Node `>=22.19`；tmux；git（委派）
- 资产（12 个）：celagent 五平台、celld linux-x64/linux-arm64/darwin-arm64、`install.sh`/`install.ps1`/`worker.tar.gz`/`SHA256SUMS`
- 发布流水线：`.github/workflows/release.yml`；Release run `TBD`
- CI：main `TBD`（Node 22/24）
- 无凭证冒烟：`./scripts/release-smoke.sh v3.0.0`（待发布后跑）
- 3.0 核心：delegate spawn/wait/send/list、子 agent 独立 worktree、父死子不死、interrupted 不重放

## 4. 版本里程碑

| 版本 | 内容 | 状态 |
|---|---|---|
| 0.4.3 | 最后 0.x：Pi JSONL 权威、整文件 CAS | 历史 |
| 1.0.0 | 分段 JSONL + manifest CAS + journal + lease + outbox + workspace + headless + import-v0 | 历史 |
| **2.0.0** | tmux supervisor/watchdog + attach/detach + 增量工作区检查点 + usage + trigger preview | 历史 |
| **3.0.0** | durable delegation：delegate spawn/wait/send/list + 子 agent 独立 worktree/session + 父死子不死 | 已发布 |
| 3.1 | cron/webhook/审批/预算、services.yaml、FTS/记忆、语义 UI、子 agent TUI attach、真实后端矩阵 | 候选 |

## 5. 工程约定

1. 中文注释/文档/commit。
2. 先分析方案再动手；改动说明影响面。
3. 数据必须真实；测试/演示数字来自实测。
4. 安全红线：不提交 key/凭证；不写真实用户名/绝对路径/内部项目名/真实 bucket/session；凭证运行时获取。
5. 提交作者 `hxddh <hxddh@users.noreply.github.com>`。
6. 改完跑 `npm test` + `node --check` + `bash -n install.sh setup.sh scripts/*.sh` + secret/PII 扫描。
7. 不要重新引入：
   - `sessions/<id>.json` 读写 / steer 注入 / 隐式迁移；
   - worker 模拟任务 / `celagent task`；
   - 按 bucket 前缀批量删 `own.json`；
   - `CELLD_STORAGE_PROBE`；
   - 旧 `/__celld/health`；
   - 源码字符串锚定代替行为测试；
   - 客户端进程内 lease/append（2.0 由 supervisor 持有）。

## 6. 已知债 / 边界

- **单写者**：不做并发多写；跨机顺序接管。
- **任意副作用 exactly-once 不承诺**；非幂等工具崩溃后 `unknown`。
- **工作区检查点**：首次全量可能慢；不保存进程内存/socket/数据库；excluded 目录由用户重建。
- **本地 supervisor**：笔记本休眠暂停；真正 always-on 需要远程主机或 celld 常驻。
- **trigger preview**：只支持 `--every`；无 cron/webhook、无硬预算、无审批。
- **celld**：容器磁盘临时、迁移停容器；3.0 不宣称实时迁移。
- **Pi 1.5.2**：扩展/事件 API 仍演进；精确钉版本。
- **真实后端**：BOS/R2/S3 长时间压测、1 GB/20k 文件 benchmark、多机分区未在 CI 跑。
- **子 agent 是 headless**：没有 TUI attach；人用 `logs` / `delegate send` 接管；attach 只用于 `up` 会话。
- **子 agent 不是沙箱**：共享父进程环境变量与模型 key；只派发信任的任务。
- **不自动合并/推送**：子 agent 只提交本地分支；合并与 push 由人做。
- **父子控制面是本地文件**：换机器恢复子 session 需要重建 delegation 目录，或直接 `up <child>`。
- **FTS/语义 UI/cron/审批**：3.1。

## 7. 发版流程

1. `npm test` 全绿；`node --check`；`bash -n`。
2. 版本号：`package.json`、`bin/celagent-tui.mjs`、`install.sh` 三处一致。
3. `npm pack --dry-run` 检查文件。
4. commit → push main。
5. tag `v3.0.0` → push tag；Release workflow 构建资产。
6. `./scripts/release-smoke.sh v3.0.0` 验证下载/SHA/version/help。
7. 更新本文 §3 的 Release 链接与资产状态。

## 8. 相关文档

- `README.md` — 用户安装/命令/保证
- `docs/architecture.md` — 架构与 ADR
- `docs/v3.0.md` — 3.0 实现合同与验证记录
- `docs/v2.0.md` — 2.0 实现合同与验证记录
- `docs/distributed-deployment.md` — 多机/接管
- `docs/s3-compat-evaluation.md` — 合格后端/CAS 门禁
- `docs/bos-compat.md` — BOS 实测边界（历史，仍适用）
- `docs/v1.0.md` — 1.0 历史合同
- `docs/archive/` — 0.x 历史文档，不要据此改代码
