> **Archived:** this is the celagent 4.1 agent-computer documentation.
> The current product in this repository is [Lenslet](../lenslet/README.md).

# celagent 4.1

**Durable agent computer** — 客户端只是窗口，执行在工作区里。4.1 让 goal 跑在一个受 supervisor 看护的 runner 里：给一个目标和一条验收命令，runner 每轮跑验收，没通过就让 agent 带着失败输出再试，直到通过、超预算或需要人。agent 是 runner 的子进程，不是宿主。

```text
goal：目标 + 验收命令
  → agent 尝试 → 验收命令（agent 之外）
      → 失败：输出喂回，继续
      → 通过：done
      → 预算 / 墙钟 / 同一失败 ×3 / 改测试：停或问人
合并永远由人决定
```

4.1 保持 3.0 的底线：子 agent 是完整 session；会话权威是对象存储里的 manifest + segments；单写者 lease + epoch；不承诺任意副作用 exactly-once。

---

## 4.1 变了什么

4.0 的 goal loop 是一个 Pi 扩展，长在 agent 的 TUI 里；4.1 把它彻底换成独立 runner。

| 维度 | 4.0 | 4.1 |
|---|---|---|
| 循环宿主 | agent 的 Pi 扩展 | **受 supervisor 看护的 `__goal`** |
| agent 形态 | TUI + `sendMessage` | **每次 `celagent run`（headless）** |
| 超时/预算 | turn 边界 | **杀进程组 + 2 秒预算轮询** |
| 停止 | Esc 容易被忽略 | **`goal stop` 杀 attempt/check** |
| 观察 | attach 到 goal TUI | **`logs` + `status`** |
| 委派 | goal 内可 delegate（未接好） | **goal 内禁用 delegate** |
| 停止条件 | 模型说 done | **验收命令 exit 0（不变）** |

1.0 把会话做成可恢复；2.0 把执行者和工作区做成可恢复；3.0 把委派树做成可恢复；4.1 把“意图”做成可恢复——目标不会因为一次失败、一次崩溃或一次 Esc 而丢失。

---

## 安装

前置：

- Node.js **>= 22.19**
- `tmux`（`up` / `attach` / `delegate` / `goal` 需要；`run` / `list` / `workspace` 不需要）
- `git`（3.0 委派 / 4.1 goal 需要：默认从 `HEAD` 建 worktree）
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

## 委派子 agent

在 TUI 里直接说“把这三个独立测试分头修了，各自开 worktree”，模型会调用 `delegate`。也可以手动：

```bash
celagent delegate spawn "只修 tests/a.test.mjs" --cwd ~/repo
celagent ps --tree
celagent delegate list --all
celagent logs dlg-...
celagent delegate send dlg-... "改用另一种方案"
celagent delegate wait dlg-...
```

- 每个子 agent 从 `HEAD` 创建独立 worktree 和分支 `celagent/<parent>/<child>`；父会话未提交的改动不会带过去。
- 子 agent 不能再 `delegate`，也不会 push；结束时在自己的分支上提交，返回 commit / diff / tokens / cost。
- `wait` 超时只返回 `running`，不取消；`send` 在下一个 turn 边界生效。
- 子 agent 是 headless：人用 `logs` / `send` 接管；`attach` 仍用于 `up` 会话。
- 并发上限默认 4（`celagent config set delegate.maxChildren 2`，最大 8）；建议先跑 2 个。
- 合并由人决定：`git cherry-pick <commit>` 或 `git merge celagent/<parent>/<child>`。

---

## 目标循环

```bash
celagent goal start "把 npm test 修绿" --accept "npm test" --cwd ~/repo
celagent goal status
celagent logs <goal-id>            # 看 runner / attempt 输出
celagent goal answer <id> "允许改测试"
celagent goal stop <id>
```

- 只有 `--accept` 命令 exit 0 才算完成；验收在 agent 之外跑。
- goal 是 **headless**：没有 `attach` 到 goal 的 TUI。用 `logs` 看进度、`goal answer` 回答；要看 TUI 先 `goal stop`，再 `celagent up <id> --cwd <goal worktree>` 接管同一个 session。
- 默认创建 worktree + 分支 `celagent/goal/<id>`；`--in-place` 才写当前目录；goal 从 repo 根启动（子目录请用 `--in-place`）。
- 每轮只把最近一次失败输出喂回；agent 需要人时输出 `BLOCKED: <问题>`，goal 转 blocked，不烧 token；`goal answer` 的回答会进入下一次 prompt。
- 硬停：runner 在 deadline/预算到点时杀 agent/验收的进程组；同一失败 ×3、改保护路径转 blocked。
- 默认保护 `tests/`、`test/`、`*.test.*`、`*.spec.*`；改了要人回答“允许”。
- goal 内不提供 `delegate`：goal 是单 session 追一个目标；并行请用 3.0 的 delegate。
- 通过后仍由人看 diff、合并；不自动 push/merge。

---

## 命令

| 命令 | 作用 |
|---|---|
| `celagent` | 启动/附着新会话的 supervisor（默认） |
| `celagent <id>` | 启动/附着指定会话 |
| `celagent up [id] [--cwd dir] [--restore] [--attach]` | 后台启动 supervisor |
| `celagent attach <id>` | 附着；关终端 = detach |
| `celagent down <id>` | 停止 supervisor（数据保留） |
| `celagent ps` | 列出 supervisor / watchdog；`--tree` 按父子关系 |
| `celagent logs <id>` | 查看 supervisor / 子 agent 日志 |
| `celagent delegate spawn "<task>"` | 派一个子 agent（默认独立 worktree） |
| `celagent delegate list [--all]` | 列出子 agent 与状态 |
| `celagent delegate wait <id>` | 等子 agent 结束（超时返回 running） |
| `celagent delegate send <id> "<msg>"` | 在 turn 边界追加指令 |
| `celagent goal start "<objective>" --accept "npm test"` | 目标循环；验收通过才停 |
| `celagent goal status [id]` / `list` | 进度、成本、最近验收、diff |
| `celagent goal answer <id> "<answer>"` | 回答 blocked 问题 |
| `celagent goal stop <id>` / `resume <id>` | 停止 / 重启 runner |
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

**3.0 委派**：

```text
父 session（celagent-<parent>）
  └─ delegate tool
       ├─ 子 session A：tmux + worktree A + branch celagent/<parent>/<childA>
       ├─ 子 session B：tmux + worktree B + branch celagent/<parent>/<childB>
       └─ 子 session C：...
              │
              ▼
        同一个 v2 engine + 同一个 bucket
        sessions/<child>/manifest.json + segments + journal + workspace
```

父子的本地协议在 `~/.config/celagent/delegations/<child>/`：

```text
task.md        任务（只读）
state.json     running / prompted / promptDone / turns
result.json    done|interrupted|error + text/commit/diff/cost
inbox/         send 追加的消息（FIFO）
output.log     子 agent 的可读输出（celagent logs）
```

这些文件是本地控制面；会话本身仍以对象存储的 manifest 为权威。子 agent 的 worktree 在 `~/.config/celagent/worktrees/<child>/`，分支由 git 保存。

**4.1 目标 runner**：

```text
tmux supervisor（celagent-<goalId>）
  └─ __goal runner
       ├─ 验收：/bin/sh -c <accept>（独立进程组，超时杀组）
       ├─ 尝试：celagent run --session <id>（headless，独立进程组）
       └─ goals/<id>/{goal.json,GOAL.md,checks,attempt.json,output.log,STOP}
```

goal runner 由 2.0 的 tmux + watchdog 看护；agent 是它每次拉起的子进程。session 仍是普通 2.0 session；goal 停止后可以用 `celagent up <id> --cwd <goal worktree>` 接管。

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
- 子 agent 被 `kill -9`：watchdog 拉起同一个 `__child`；如果 turn 已标记开始但没结束，写 `interrupted`，**不重放 prompt**。
- 父 session 被 `kill -9`：子 agent 不受影响；父恢复后 `delegate list` 找到子 session，`delegate wait` 取回结果。
- 子 agent 正常结束：写 `result.json`，写 stop 标记，watchdog 退出；worktree 和分支保留，等人工合并。
- goal runner 被 `kill -9`：watchdog 拉起 `__goal`；先清 attempt/check 的残留进程组，再重跑验收；journal 的未完成工具仍是 `unknown`，不重放。
- goal 在 blocked 时崩溃：重启后继续等 `answer.json`，不烧 token。
- `goal stop`：写 STOP、杀 attempt/check 进程组、停 supervisor；`goal resume` 清 STOP、清残留、重启 runner。
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
- 检查点不保存进程内存、打开的 socket、数据库运行时；用 `.celagent/services.yaml`（3.1）声明恢复后要启动的服务。
- 加密开启时，blob 和 checkpoint 都走 `CELAGENT_ENCRYPTION_KEY` 的 AES-256-GCM；**key 丢失 = 数据不可读**。

---

## 触发器（preview）

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

每次 trigger 运行使用新的 session（`trg-<id>-<timestamp>`），不会抢用户 TUI 的租约。当前只支持 `--every` 分钟；cron/webhook、预算硬停、审批和通知在 3.1。触发器默认拥有和 `run` 相同的工具权限，建议用独立 worktree/cwd，不要在有未提交改动的目录里跑无人值守写操作。

---

## 远程 / SSH

3.0 不要求 celld。把同一份 `celagent` 装在任意 Linux 主机上：

```bash
ssh agent-host 'celagent up fix-auth --cwd ~/repo'
ssh -t agent-host 'celagent attach fix-auth'
```

工作区检查点和会话都在对象存储，主机只是 compute。celld 0.6.3 继续作为可选的 fleet/容器后端（preview）；它的容器磁盘是临时的，迁移会停容器，所以 3.0 只做 checkpoint/restore，不做实时迁移。

---

## 从 4.0 升级

- 4.1 继续读 4.0 的 session/manifest/checkpoint；已有会话不需要迁移。
- 4.0 的 goal 扩展被删除；`goal start/status/answer/stop/resume` 命令保留。已有的 goal 用 `goal stop` → `goal resume` 切到新 runner。
- **goal 不再是 TUI**：`attach` 不适用于 goal；用 `logs` / `goal answer`，停止后用 `up <id> --cwd <goal worktree>` 接管 session。
- `goal` 内不再提供 `delegate`；需要并行先用 3.0 的 `delegate`，再让 goal 处理单个分支。
- 加密、bucket、profile、region 配置不变；跨机必须同一个 key。
- 3.0/2.0 的旧说明仍在 `docs/v3.0.md`、`docs/v2.0.md`。

---

## 测试

```bash
npm test
```

- `tests/v2.test.mjs`：分段写/恢复、hash 校验、租约/接管、outbox、journal、加密、import、旧 workspace。
- `tests/v2-checkpoint.test.mjs`：增量检查点、blob 复用、symlink、force 清理、runner 去抖。
- `tests/v3-child.test.mjs`：子 agent 状态机（task / inbox / interrupted / finalize）。
- `tests/v3-worktree.test.mjs`：worktree 创建/提交/清理、父仓库脏改动不带入。
- `tests/v3-delegate.test.mjs`：spawn / wait / send / list、并发上限、非 git 回退。
- `tests/v4-goal.test.mjs`：验收 exit/output/timeout、五种守卫、保护路径、状态与 prompt。
- `tests/v4-runner.test.mjs`：goal runner 循环（基线、重试、blocked/回答、预算/墙钟、BLOCKED）。
- `tests/v2-supervisor.test.mjs`：tmux supervisor 启动/停止/watchdog/重复启动/child kind。
- `tests/v2-triggers.test.mjs`：trigger 配置/到期/删除。
- `tests/v2-usage.test.mjs`：Pi usage → manifest.stats。
- `tests/contract.test.mjs`：版本/依赖/模块/CLI 合同。
- 需要 `celld` / 真实桶的用例无节点时 skip。

本地 S3 兼容验证（moto）见 `docs/v2.0.md`。

---

## 边界

- **单写者**：不做并发多写；跨机是顺序接管。
- **不承诺任意副作用 exactly-once**；非幂等工具崩溃后标 `unknown`。
- **笔记本休眠时本地 compute 暂停**；真正的 always-on 需要远程主机或 celld 常驻。
- **首次检查点可能慢**：大量文件首次上传受对象存储/CLI 延迟影响；之后是增量。
- **触发器是 preview**：无 cron/webhook、无硬预算、无审批；3.1 补齐。
- **子 agent 是 headless**：不能 attach TUI；用 `logs` / `delegate send` 接管。
- **子 agent 不是沙箱**：共享父进程的模型 key 与环境变量；只派发信任的任务。
- **不自动合并/推送**：子 agent 只提交本地分支，合并与 push 由人做。
- **加密是整桶模式**；同一 bucket 不要混用加密/非加密写入。
- **Pi 精确钉 1.5.2**；升级需跑 TUI + headless + journal + checkpoint 冒烟。
- Windows 没有 tmux supervisor；`run`/`list`/`workspace` 仍可用。

## 文档

- [`docs/architecture.md`](docs/architecture.md) — 架构与数据流
- [`docs/v4.1.md`](docs/v4.1.md) — 4.1 实现合同与验证
- [`docs/v3.0.md`](docs/v3.0.md) — 3.0 实现合同与验证
- [`docs/v2.0.md`](docs/v2.0.md) — 2.0 实现合同与端到端验证
- [`docs/distributed-deployment.md`](docs/distributed-deployment.md) — 多机/接管
- [`HANDOFF.md`](HANDOFF.md) — 接手入口
- [`docs/v1.0.md`](docs/v1.0.md) — 1.0 历史合同

## License

MIT
