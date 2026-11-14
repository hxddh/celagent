# celagent 2.0 架构

> 2.0 = durable agent computer。1.0 把会话/工具调用做成可恢复；2.0 把执行者和工作区也做成可恢复。
> 配套：`README.md`（用户）、`docs/v2.0.md`（实现合同/验证）、`HANDOFF.md`（接手）。

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
- 不做 cron/webhook/审批/硬预算；2.1 补。

## 9. 2.0 边界

- 不做并发多写；跨机是顺序接管。
- 不承诺任意副作用 exactly-once；只承诺 journal + 幂等键 + `unknown`。
- 不承诺实时迁移；celld 容器移动会停容器，只能 checkpoint/restore。
- 不保存进程内存、socket、数据库运行时；服务由未来 `.celagent/services.yaml` 重启。
- 本地 supervisor 受笔记本休眠影响；真正 always-on 用远程 Linux 主机或 celld 常驻。
- 触发器 preview 无审批；无人值守写操作请用独立 worktree/cwd。

## 10. ADR

| 决策 | 选择 | 原因 |
|---|---|---|
| UI 附着 | PTY/tmux，不重写 Pi TUI | 保留 Pi 的键位/扩展/审批；语义 UI 留 2.1 |
| 执行者 | supervisor 进程 | 客户端死亡不影响执行 |
| 工作区 | 内容寻址增量 blob，不依赖 restic | 无额外二进制；与现有加密/存储适配器一致 |
| 并发 | 单写者 lease | 避免多写冲突；顺序接管已够用 |
| 远程 | SSH 主机 + celld preview | 不把 2.0 绑在 celld beta 上 |
| 版本 | 2.0 破坏默认 CLI 行为 | `celagent` 从进程内 TUI 变为 up+attach；`--foreground` 保留旧路径 |
