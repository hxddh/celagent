# HANDOFF — celagent 1.0

> 唯一权威交接入口。先读本文，再读 `docs/architecture.md`（架构）与 `docs/v1.0.md`（实现合同/验证）。
> 用户视角见 `README.md`。旧 v0.3/v0.4 文档已移到 `docs/archive/`，不要按它们改当前代码。

## 0. 项目定位

**celagent 1.0 = durable agent runtime。**

- Pi 1.5 负责 agent 语义（LLM、工具、compaction、TUI）；
- celagent 负责把每个 step 变成对象存储里的不可变对象；
- 会话权威是 v2 manifest + segments；工具副作用先写 journal；
- 单写者 lease + epoch，跨机可接管；
- 工作区可快照/恢复；
- celld 0.6.3 是可选执行底座，核心不依赖。

保证：

```
已提交 step 不丢
进行中的工具最多一个 unknown
有幂等键的副作用不重复
没有幂等键的副作用绝不静默重放
```

## 1. 代码地图

| 路径 | 职责 |
|---|---|
| `bin/celagent-tui.mjs` | CLI/TUI/headless：命令解析、v2 装配、TUI runtime、`run`、`status`、`import-v0`、`workspace` |
| `src/v2/session.js` | 分段 JSONL：append、materialize、readSession、list、delete、importV0 |
| `src/v2/manifest.js` | manifest 校验 / 读 / CAS 写 |
| `src/v2/lease.js` | 单写者租约、epoch、同机死 pid 接管、心跳 |
| `src/v2/outbox.js` | 本地 fsync 发件箱；失败留到下次启动 |
| `src/v2/journal.js` | 工具 journal start/finish/reconcile |
| `src/v2/extension.js` | Pi 1.5 durability extension（`tool_call` block / `tool_result`） |
| `src/v2/adapter.js` | 对象存储适配器（BOS/S3 经 aws CLI；内存 store 测试） |
| `src/v2/crypto.js` | 可选 AES-256-GCM 信封加密 |
| `src/v2/workspace.js` | tar.gz 工作区快照/恢复 |
| `src/v2/status.js` | durable seq / lease / outbox 状态 |
| `src/bos.js` | aws CLI 原语：get/put/list/delete、CAS 探针、endpoint 门禁 |
| `src/bos-tools.js` | `history_search` / `session_snapshot`（v2） |
| `src/persist.js` | 旧格式工具函数（`jsonlFromTurns` 仍被 `import-v0` 用）；旧队列/legacy 路径不在 1.0 运行时 |
| `worker/src/index.js` | celld 可选缓存 worker（checkpoint/resume/sync/hibernate/wake；无模拟任务） |
| `scripts/node_mgr.sh` | 本机 celld 节点管理（0.6.3 health 路径） |
| `scripts/cluster_mgr.sh` | 多机 celld 节点管理（可选） |
| `install.sh` / `setup.sh` | 安装 / 部署（celld 0.6.3，不再传 CELLD_STORAGE_PROBE） |
| `tests/v2.test.mjs` | 分段/租约/outbox/journal/加密/import/workspace 行为 |
| `tests/v1-contract.test.mjs` | 1.0 发布合同（版本/依赖/health/CLI） |
| `tests/persist.test.mjs` | 旧 JSONL 转换/谱系/尾行（import-v0 仍用） |
| `tests/core.test.mjs` | CLI + 可选 celld/BOS |
| `tests/cas-probe.test.mjs` | CAS 探针语义 |
| `tests/p0-p2-bugs.test.mjs` | endpoint/store_env 回归 |

## 2. 开发与验证

```bash
node --version                 # >= 22.19
npm install
npm test                       # 无 celld/真实桶时相关用例 skip

# 本地 S3 兼容（moto）端到端
# 需要 aws CLI；见 docs/v1.0.md §10
node bin/celagent-tui.mjs doctor
node bin/celagent-tui.mjs run --session demo --prompt "hello" --json
node bin/celagent-tui.mjs status demo
```

环境事实：

- **BOS 默认**：`https://s3.bj.bcebos.com`，region `bj`，profile `bos`。
- **aws CLI 是硬依赖**：`install.sh` 会检查；缺失时 `doctor` 明确报错，不再把 ENOENT 当网络抖动。
- **Pi 精确 1.5.2**：不要升 caret；升级要跑 TUI + headless + journal 冒烟。
- **celld 0.6.3**：health 是 `/.well-known/celld/health`；`CELLD_STORAGE_PROBE` 已移除；不要按 bucket 前缀删 `own.json`。
- **加密**：`CELAGENT_ENCRYPTION_KEY` 32 字节 base64；整桶模式；key 丢失不可读。

## 3. 发布状态

- 版本：**1.0.0**
- 依赖：pi `1.5.2`（exact）；celld `0.6.3`（随包）；Node `>=22.19`
- 资产：celagent 五平台 + celld linux-x64/linux-arm64/darwin-arm64 + `install.sh`/`install.ps1`/`worker.tar.gz`/`SHA256SUMS`
- 发版：`.github/workflows/release.yml` 在 tag `v*` 时构建并上传
- 无凭证冒烟：`./scripts/release-smoke.sh v1.0.0` 或 `latest`

## 4. 版本里程碑

| 版本 | 内容 | 状态 |
|---|---|---|
| 0.4.3 | 最后 0.x：Pi JSONL 权威、整文件 CAS | 历史 |
| **1.0.0** | 分段 JSONL + manifest CAS + journal + lease + outbox + workspace + headless + import-v0；pi 1.5.2；celld 0.6.3 | 本刀 |
| 1.1 | TUI attach supervisor；celld container workspace；FTS 索引；真实崩溃注入/后端矩阵 | 候选 |

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
   - 源码字符串锚定代替行为测试。

## 6. 已知债 / 边界

- **单写者**：不做并发多写；跨机顺序接管。
- **任意副作用 exactly-once 不承诺**；非幂等工具崩溃后 `unknown`。
- **LLM 流中断重发**，可能重复 token。
- **workspace**：opt-in tar 快照，不是实时同步；容器化 1.1。
- **加密**：整桶；不能混用；key 管理是用户责任。
- **Pi 1.5.2**：扩展/事件 API 仍演进；精确钉版本。
- **真实后端**：BOS/R2/S3 的长时间压测/多机故障注入尚未在 CI 跑。
- **FTS 索引**：1.0 仍是按需读 segment；1.1 再做。
- **TUI supervisor**：1.0 的 TUI 仍是进程内 runtime；headless `run` 已 step-durable，TUI 的 `/quit`/信号会 flush + release，但 TUI 被 kill -9 时 agent 也随之结束。1.1 做 attach。

## 7. 发版流程

1. `npm test` 全绿；`node --check`；`bash -n`。
2. 版本号：`package.json`、`bin/celagent-tui.mjs`、`install.sh` 三处一致。
3. `npm pack --dry-run` 检查文件。
4. commit → push main。
5. tag `v1.0.0` → push tag；Release workflow 构建资产。
6. `./scripts/release-smoke.sh v1.0.0` 验证下载/SHA/version/help。
7. 更新本文 §3 的 Release 链接与资产状态。

## 8. 相关文档

- `README.md` — 用户安装/命令/保证
- `docs/architecture.md` — 架构与 ADR
- `docs/v1.0.md` — 实现合同与 2026-11-14 验证记录
- `docs/distributed-deployment.md` — 多机/接管
- `docs/s3-compat-evaluation.md` — 合格后端/CAS 门禁
- `docs/bos-compat.md` — BOS 实测边界（历史，仍适用）
- `docs/archive/` — v0.3/v0.4 历史文档，不要据此改代码
