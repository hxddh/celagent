# celagent 2.0

**Durable agent computer** — 客户端只是窗口，执行在工作区里。Pi 1.5 TUI 跑在 tmux supervisor 中；工具调用有 journal，工作区有增量检查点，会话有 manifest/lease。关终端、断网、进程被 kill，工作继续；下次 `attach` 或从另一台机器 `up --restore` 接上。

```text
客户端断开 / 终端关闭，supervisor 继续执行
工具结果后自动生成增量工作区检查点
崩溃后从最后一个检查点和 manifest 恢复
进行中的工具最多一个 unknown；没有幂等键的副作用绝不静默重放
```

---

## 2.0 变了什么

| 维度 | 1.0 | 2.0 |
|---|---|---|
| 执行者 | TUI 客户端进程 | **tmux supervisor + watchdog** |
| 客户端断开 | 执行停止 | **detach；supervisor 继续** |
| 工作区 | 手动全量 tar，500 MB 上限 | **自动增量检查点（内容寻址 blob）** |
| 崩溃恢复 | 会话/manifest 恢复 | **会话 + 工作区检查点 + 未知工具标记** |
| 成本 | `stats` 恒为 0 | **Pi usage 汇总 tokens/cost** |
| 触发器 | 无 | **本地 interval trigger daemon（preview）** |
| 远程 | 同 bucket 手工续写 | **SSH 到 Linux 主机跑同一 supervisor** |

2.0 仍然保持 1.0 的底线：会话权威是对象存储里的 manifest + segments；Pi 的 JSONL 是语义源；单写者 lease + epoch；不承诺任意副作用 exactly-once。

---

## 安装

前置：

- Node.js **>= 22.19**
- `tmux`（`up` / `attach` 需要；`run` / `list` / `workspace` 不需要）
- `aws` CLI（BOS/S3/R2 的签名与条件写；`install.sh` 会检查）
- 一个支持条件写的 bucket：BOS（默认）、AWS S3、Cloudflare R2、Tigris
- LLM：`DEEPSEEK_API_KEY` / `OPENAI_API_KEY` / `ANTHROPIC_API_KEY`，或 Pi `/login`

```bash
# 一键安装（GitHub Release 二进制 + 随包 celld 0.6.3）
curl -fsSL https://github.com/hxddh/celagent/releases/latest/download/install.sh | sh

# 或源码模式（改即生效）
CELAGENT_SRC=~/celagent ./install.sh

# 首次部署：建/复用 bucket、CAS 探针、celld 节点、写 settings
CELAGENT_SRC=~/celagent ./setup.sh

export DEEPSEEK_API_KEY=sk-...
celagent up demo --cwd ~/repo --attach
```

非 BOS：

```bash
celagent config set persistence.endpoint https://<account>.r2.cloudflarestorage.com
celagent config set persistence.region auto
celagent config set persistence.profile r2
celagent doctor          # CAS 探针必须通过
```

---

## 快速开始：关掉终端，工作继续

```bash
# 1. 在一个仓库里启动 supervisor 并附着
celagent up fix-auth --cwd ~/repo --attach

# 2. 按 Ctrl-b d detach（不是 /quit）
#    或者直接关掉终端；tmux 里的 Pi 仍在跑

# 3. 回来
celagent attach fix-auth

# 4. 停止 supervisor；会话和检查点保留
celagent down fix-auth
```

另一台机器（同一个 bucket / 同一个 `CELAGENT_ENCRYPTION_KEY`）：

```bash
celagent up fix-auth --cwd ~/repo2 --restore --attach
```

`--restore` 会把最近的工作区检查点恢复到 `~/repo2`，再启动 supervisor；不会自动搬进程内存或打开的 socket。

---

## 命令

| 命令 | 作用 |
|---|---|
| `celagent` | 启动/附着新会话的 supervisor（2.0 默认） |
| `celagent <id>` | 启动/附着指定会话 |
| `celagent up [id] [--cwd dir] [--restore] [--attach]` | 后台启动 supervisor |
| `celagent attach <id>` | 附着；关终端 = detach |
| `celagent down <id>` | 停止 supervisor（数据保留） |
| `celagent ps` | 列出 supervisor / watchdog |
| `celagent logs <id>` | 查看 supervisor 日志 |
| `celagent run --prompt "..."` | 无头运行；`--session <id>`、`--json`、`--cwd` |
| `celagent list` | 列出 v2 会话；旧格式单独标注 |
| `celagent export <id>` | 导出 Pi JSONL |
| `celagent rm <id> --yes` | 删除会话（v2 + 旧对象） |
| `celagent import-v0 <id> --yes` | 旧 `.jsonl`/`.json` 一次性导入 |
| `celagent status [id]` | durable seq / segments / lease / outbox / workspace / stats |
| `celagent workspace checkpoint <id>` | 手动生成增量检查点 |
| `celagent workspace list <id>` | 列出检查点 |
| `celagent workspace restore <id> [--checkpoint id] [--cwd dir] [--force]` | 恢复检查点 |
| `celagent workspace status <id>` | 当前检查点指针 |
| `celagent trigger add <id> --prompt ... --every 60` | 本地 interval 触发器 |
| `celagent trigger list/rm/run <id>` | 管理/立即运行 |
| `celagent trigger daemon --start/--stop/--status` | 后台调度器 |
| `celagent doctor` | 配置 / aws / tmux / CAS / Pi / 可选 celld |
| `celagent cas-probe` | 条件写探测（exit 2 = 未完成，无法判定） |
| `celagent config get/set` | 配置读写 |

TUI 内：`/new`、`/fork`、`/resume`、`/compact` 仍由 Pi 提供。`/quit` 会停止 supervisor；**detach 用 `Ctrl-b d` 或直接关终端**。

---

## 架构

```text
Client (TUI / CI / trigger)
      │  tmux attach / detach
      ▼
tmux supervisor (celagent-<id>)
  ├─ watchdog：进程被 kill 后重新拉起，TUI 从 manifest/checkpoint 恢复
  ├─ Pi 1.5 TUI
  │    └─ durability extension：tool_call journal.start / tool_result journal.finish
  └─ appender + usage
      │
      ▼
celagent v2 engine
  ├─ lease：单写者 + epoch；心跳续租；过期可接管
  ├─ outbox：~/.config/celagent/outbox，先 fsync 本地再上云
  ├─ segments：按 entry 分块，不可变，hash 校验
  ├─ manifest：CAS 提交 head / segments / workspace / stats
  └─ workspace checkpoint：文件 SHA-256 去重，只上传变化 blob
      │
      ▼
对象存储（BOS / S3 / R2 / Tigris）
  sessions/<id>/manifest.json
  sessions/<id>/segments/000001-000005-<hash>.jsonl
  sessions/<id>/journal/<toolCallId>.json
  sessions/<id>/workspace/checkpoints/<hash>.json
  sessions/<id>/workspace/blobs/<sha256>
```

**读路径**：`celagent <id>` → `materialize` 拉 manifest + segments → 写本地 Pi JSONL → `SessionManager.open`。每个 segment 校验 SHA-256；manifest head 必须与最后一个 entry 一致。

**写路径**：`message_end` / `turn_end` / `session_compact` → 追加不可变 segment → CAS manifest → 清 outbox；工具 `tool_call` 先写 journal，失败则 block 工具（fail-closed）。

**工作区检查点**：工具结果/append 成功后去抖 500 ms，扫描工作区；未变化文件复用上一次 hash，变化文件上传为 blob；checkpoint 元数据写对象存储；manifest CAS 指向最新 checkpoint。默认忽略 `.git`、`node_modules`、`.venv`、`dist`、`build`、`.cache`、`target`、`vendor`、`.amp`。

---

## 崩溃 / 接管语义

- 正常 detach：tmux 保留 TUI，supervisor 继续；lease 心跳续租。
- `/quit` / `celagent down`：停止 supervisor，flush outbox + 最终检查点，释放 lease。
- TUI 被 `kill -9`：watchdog 2 秒内重新拉起；同机死 pid 可立即接管，新 TUI 从本地 JSONL/远端 manifest 恢复，并生成最终检查点。
- 跨机：同一 bucket，旧机 lease 过期（15 秒）后 epoch+1 接管；旧写者的 manifest CAS 会失败（fencing）。
- 进行中的工具：journal 有 start 无 finish → `unknown`；**不静默重放**。有幂等键的副作用由调用方保证不重复。
- 工作区可能比 manifest 落后：`status` 显示 `workspace.seq`；如果检查点上传失败，会保留警告，下一次触发重试。没有静默的“假装已保存”。

---

## 工作区检查点

```bash
celagent workspace checkpoint fix-auth --cwd ~/repo
celagent workspace list fix-auth
celagent workspace restore fix-auth --cwd ~/repo2 --force
```

- 内容寻址：文件 SHA-256 决定 blob 名；同一 blob 在多轮/多会话间可复用。
- 首次检查点是全量上传；之后只上传变化 blob。单文件默认上限 128 MB，总工作区默认上限 1 GB。
- 检查点不保存进程内存、打开的 socket、数据库运行时；用 `.celagent/services.yaml`（2.1）声明恢复后要启动的服务。
- 加密开启时，blob 和 checkpoint 都走 `CELAGENT_ENCRYPTION_KEY` 的 AES-256-GCM；**key 丢失 = 数据不可读**。

---

## 触发器（2.0 preview）

```bash
celagent trigger add nightly \
  --prompt "triage open issues and summarize" \
  --cwd ~/repo \
  --every 60

celagent trigger run nightly          # 立即跑一次
celagent trigger daemon --start       # 每 60 分钟检查一次
celagent trigger daemon --status
celagent trigger daemon --stop
```

每次 trigger 运行使用新的 session（`trg-<id>-<timestamp>`），不会抢用户 TUI 的租约。当前只支持 `--every` 分钟；cron/webhook、预算硬停、审批和通知在 2.1。触发器默认拥有和 `run` 相同的工具权限，建议用独立 worktree/cwd，不要在有未提交改动的目录里跑无人值守写操作。

---

## 远程 / SSH

2.0 不要求 celld。把同一份 `celagent` 装在任意 Linux 主机上：

```bash
ssh agent-host 'celagent up fix-auth --cwd ~/repo'
ssh -t agent-host 'celagent attach fix-auth'
```

工作区检查点和会话都在对象存储，主机只是 compute。celld 0.6.3 继续作为可选的 fleet/容器后端（preview）；它的容器磁盘是临时的，迁移会停容器，所以 2.0 只做 checkpoint/restore，不做实时迁移。

---

## 从 1.0 升级

- 2.0 继续读 1.0 的 v2 manifest/segments/journal；`workspace` 字段会从 tar 指针升级为 `format: "cas-v1"` 的检查点指针。
- 旧 `.jsonl`/`.json` 仍用 `celagent import-v0 <id> --yes` 一次性导入。
- `celagent` 默认行为从“进程内 TUI”改为“启动/附着 supervisor”；需要旧的进程内行为用 `celagent --foreground <id>`。
- 加密、bucket、profile、region 配置不变；跨机必须同一个 key。

---

## 测试

```bash
npm test
```

- `tests/v2.test.mjs`：分段写/恢复、hash 校验、租约/接管、outbox、journal、加密、import、旧 workspace。
- `tests/v2-checkpoint.test.mjs`：增量检查点、blob 复用、symlink、force 清理、runner 去抖。
- `tests/v2-supervisor.test.mjs`：tmux supervisor 启动/停止/watchdog/重复启动。
- `tests/v2-triggers.test.mjs`：trigger 配置/到期/删除。
- `tests/v2-usage.test.mjs`：Pi usage → manifest.stats。
- `tests/v2-contract.test.mjs`：版本/依赖/模块/CLI 合同。
- 需要 `celld` / 真实桶的用例无节点时 skip。

本地 S3 兼容验证（moto）见 `docs/v2.0.md`。

---

## 边界

- **单写者**：不做并发多写；跨机是顺序接管。
- **不承诺任意副作用 exactly-once**；非幂等工具崩溃后标 `unknown`。
- **笔记本休眠时本地 compute 暂停**；真正的 always-on 需要远程主机或 celld 常驻。
- **首次检查点可能慢**：大量文件首次上传受对象存储/CLI 延迟影响；之后是增量。
- **触发器是 preview**：无 cron/webhook、无硬预算、无审批；2.1 补齐。
- **加密是整桶模式**；同一 bucket 不要混用加密/非加密写入。
- **Pi 精确钉 1.5.2**；升级需跑 TUI + headless + journal + checkpoint 冒烟。
- Windows 没有 tmux supervisor；`run`/`list`/`workspace` 仍可用。

## 文档

- [`docs/architecture.md`](docs/architecture.md) — 2.0 架构与数据流
- [`docs/v2.0.md`](docs/v2.0.md) — 2.0 实现合同与端到端验证
- [`docs/distributed-deployment.md`](docs/distributed-deployment.md) — 多机/接管
- [`HANDOFF.md`](HANDOFF.md) — 接手入口
- [`docs/v1.0.md`](docs/v1.0.md) — 1.0 历史合同

## License

MIT
