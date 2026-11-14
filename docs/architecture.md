# celagent 架构

> 4.0 = durable intent：3.0 的委派之上，agent 追一个 goal，验收命令不通过就继续。
> 配套：`README.md`（用户）、`docs/v4.0.md`（4.0 合同）、`docs/v3.0.md`（3.0 合同）、`docs/v2.0.md`（2.0 合同）、`HANDOFF.md`（接手）。

## 1. 一句话

**客户端只是窗口，supervisor 才是执行者。** Pi 原版 TUI 跑在 tmux 中；tmux 由 watchdog 看护；工具 journal、lease、manifest、工作区检查点都在 supervisor 侧完成。

## 2. 角色

| 角色 | 职责 | 不是 |
|---|---|---|
| Client | `attach` / `run` / trigger；输入、渲染、断线重连 | 不持有 lease，不拥有 Pi 进程 |
| tmux supervisor | 托管 Pi TUI、保留 PTY、多客户端附着 | 不是持久化层；tmux 死了靠 checkpoint/manifest |
| watchdog | supervisor 退出后重新拉起 | 不做业务重试 |
| Worker/Session Cell（未来） | 远程协调、lease、触发 | 2.0 本地模式不需要 |
| Object storage | manifest / segments / journal / checkpoint / blob | 不执行代码 |

## 3. 一次启动

1. `celagent up <id> --cwd <dir>` 创建（或复用）tmux session `celagent-<id>`。
2. tmux 内执行 `celagent __tui <id>`；TUI 获取 lease，拉取 manifest + segments，物化本地 Pi JSONL。
3. TUI 启动 Pi；durability extension 注册 `tool_call` / `tool_result`。
4. watchdog 记录 pid；每 2 秒检查 tmux session；用户 detach 不影响它。
5. client `attach` 只是 `tmux attach`；关终端/断网 = detach。

## 4. 一次写入

```text
Pi message_end / turn_end
  → appender 读取本地 JSONL 新 entry
  → 按 256 条 / 4 MB 切 segment
  → 每个 segment If-None-Match 写对象存储
  → manifest If-Match CAS 追加 segment + head + stats
  → 清本地 outbox
  → 请求工作区检查点（500 ms 去抖）
```

工具路径：

```text
tool_call
  → journal.start：本地 fsync 后写远端；失败 block 工具（fail-closed）
tool_result
  → journal.finish：写结果；失败留本地 pending
  → 请求工作区检查点
```

## 5. 一次恢复

1. `up`/watchdog 拉起新的 TUI 进程。
2. `prepareSession`：acquire lease；同机旧 pid 已死可立即接管；跨机等 TTL。
3. `materialize`：从 manifest + segments 重建本地 JSONL；逐 segment 校验 SHA-256。
4. `journal.reconcile`：有 start 无 finish → `unknown`，不静默重放。
5. 可选 `--restore`：读 manifest 的 `workspace.latest`，下载 checkpoint/blob，恢复工作区。
6. Pi 继续；下一次工具/append 触发新检查点。

## 6. Lease / fencing

- `lease.json`：`{ owner, epoch, expiresAt }`；owner = `<hostname>-<pid>`。
- 心跳每 5 秒续租；TTL 15 秒。
- 同一 session 只有一个 supervisor；第二个 `up` 得到 `SessionLockedError`。
- 接管 epoch+1；旧写者的 manifest CAS 被 fencing 拒绝。
- checkpoint 提交前 `lease.assert`；长检查点不会因 TTL 过期而丢写（心跳）。

## 7. 工作区检查点

对象：

```text
sessions/<id>/workspace/checkpoints/<contentHash16>.json
sessions/<id>/workspace/blobs/<sha256>
```

- 扫描工作区，忽略 `.git`/`node_modules`/`.venv`/`dist`/`build`/`.cache`/`target`/`vendor`/`.amp`。
- 文件 SHA-256 命名 blob；上一次检查点的文件 hash 作为缓存，未变文件不重传。
- 同一内容复用同一 checkpoint；`manifest.workspace` 记录 `latest/seq/files/bytes/sha256/format`。
- 恢复：读 checkpoint → 并发下载 blob（4）→ 校验 hash → 写临时文件 + rename；`--force` 只清理 checkpoint 管理路径，保留 excluded 目录。
- 单文件默认 128 MB，总工作区默认 1 GB；超限明确报错，不写半成品。

## 8. 触发器（preview）

- 配置在 `~/.config/celagent/triggers/<id>.json`；`everyMinutes` 到期。
- daemon 每 30 秒检查；每次启动新的 headless `run` session；`lastPid` 存活则跳过。
- 不做 cron/webhook/审批/硬预算；3.1 补。

## 9. 2.0 边界

- 不做并发多写；跨机是顺序接管。
- 不承诺任意副作用 exactly-once；只承诺 journal + 幂等键 + `unknown`。
- 不承诺实时迁移；celld 容器移动会停容器，只能 checkpoint/restore。
- 不保存进程内存、socket、数据库运行时；服务由未来 `.celagent/services.yaml` 重启。
- 本地 supervisor 受笔记本休眠影响；真正 always-on 用远程 Linux 主机或 celld 常驻。
- 触发器 preview 无审批；无人值守写操作请用独立 worktree/cwd。

## 10. 3.0：委派

角色多了一层，但对象还是 2.0 的 session：

```text
父 session（celagent-<parent>，TUI 或 run）
  └─ delegate 工具
       ├─ spawn：git worktree + 分支 + startSupervisor(kind="child")
       ├─ wait：轮询 delegations/<child>/result.json
       ├─ send：写 inbox，下一个 turn 边界消费
       └─ list：扫 delegations/*/state.json
子 session（celagent-<child>，headless __child）
  └─ 同一个 v2 engine：manifest / segments / journal / checkpoint / lease
```

- **父会话是调度者。** 拆几个任务、谁先谁后、结果够不够好，由模型在上下文里决定；没有 mission/DAG/coordinator。
- **每个子 agent 一个 worktree。** 默认从 `HEAD` 创建分支 `celagent/<parent>/<child>`；父会话未提交改动不带入。
- **子 agent 是完整 session。** 有独立 manifest/journal/checkpoint/lease，能跨机器恢复；父进程死亡不影响它。
- **headless。** 子 agent 不提供 TUI attach；人用 `celagent logs` / `delegate send` 接管。
- **不自动合并。** 子 agent 只提交到自己的分支，返回 commit/diff；人用 git 合并。

父子控制面是本地文件 `delegations/<child>/`（task/state/result/inbox/output.log），不是新的持久化层；会话权威仍在对象存储。

## 11. 4.0：目标循环

goal 不是新 daemon，而是一个 Pi 扩展，长在普通 TUI session 里：

```text
goal session（celagent-<goalId>，TUI）
  ├─ session_start → baseline 验收
  ├─ agent_settled → 跑验收
  │     ├─ 通过 → done
  │     ├─ 失败 → sendMessage({triggerTurn}) 下一轮
  │     └─ 守卫触发 → stopped / blocked
  ├─ blocked → 等 answer.json → 注入人的回答
  └─ 验收命令：独立进程，cwd = goal worktree
```

- 停止条件从“模型说 done”变成“验收命令 exit 0”。
- 判断仍在模型上下文里；runtime 只跑验收、喂回失败、执行硬停。
- 守卫：预算、墙钟、尝试数、同一失败 ×3、保护路径被改。
- 没有 mission/DAG/coordinator/reviewer/审批状态机。
- session 仍是普通 2.0 session，可 attach、可崩溃恢复；goal 状态在 `~/.config/celagent/goals/<id>/`。

## 12. ADR

| 决策 | 选择 | 原因 |
|---|---|---|
| UI 附着 | PTY/tmux，不重写 Pi TUI | 保留 Pi 的键位/扩展/审批；语义 UI 留 3.1 |
| 执行者 | supervisor 进程 | 客户端死亡不影响执行 |
| 工作区 | 内容寻址增量 blob，不依赖 restic | 无额外二进制；与现有加密/存储适配器一致 |
| 并发 | 单写者 lease | 避免多写冲突；顺序接管已够用 |
| 远程 | SSH 主机 + celld preview | 不把 2.0 绑在 celld beta 上 |
| 版本 | 2.0 破坏默认 CLI 行为 | `celagent` 从进程内 TUI 变为 up+attach；`--foreground` 保留旧路径 |
| 委派 | 子 agent = 完整 session + worktree | 复用 2.0 的持久化；不引入调度器 |
| 子 agent UI | headless + logs/send | 保持确定性结果；TUI attach 留 3.1 |
| goal 循环 | Pi 扩展 + `agent_settled` | 不新增 daemon；保留 attach |
| 验收 | agent 之外的独立进程 | “done”是命令结果，不是模型自述 |
| goal 默认隔离 | worktree + 分支 | 不碰用户工作区；合并仍由人 |
