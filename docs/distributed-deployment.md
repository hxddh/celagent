# celagent 2.0 多机部署与接管

> 目标：同一 bucket，任何机器都能启动/接管 supervisor；单写者 lease，第二台明确报锁，过期后 epoch+1 接管；工作区从增量检查点恢复。
> 会话权威是对象存储里的 v2 manifest + segments + checkpoint；celld 是可选的执行底座，不是数据层。

## 1. 架构

```text
机器 A / B（Linux + tmux + Node + aws CLI）
  celagent up/attach/run
      │  tmux supervisor + watchdog
      │  v2 engine：segments/journal/lease/checkpoint
      ▼
对象存储（同一 bucket）
  sessions/<id>/manifest.json
  sessions/<id>/segments/*.jsonl
  sessions/<id>/journal/*.json
  sessions/<id>/lease.json
  sessions/<id>/workspace/checkpoints/*.json
  sessions/<id>/workspace/blobs/<sha256>

可选：celld 0.6.3 节点（18090/18091）
  worker 只做本地缓存/容器编排；会话读写不经过它
```

## 2. 每台机器

```bash
# Node >= 22.19 + aws CLI + tmux
CELAGENT_SRC=~/celagent ./install.sh

# 同一份 settings（bucket/endpoint/region/profile）
cat ~/.config/celagent/settings.json

celagent doctor
# 必须看到 CAS ✓；aws CLI ✓；tmux ✓
```

`persistence.bucket` 必须相同；`endpoint`/`region`/`profile` 也必须一致。加密模式下所有机器必须同一个 `CELAGENT_ENCRYPTION_KEY`。

## 3. 正常交接

```bash
# 机器 A
celagent up fix-auth --cwd ~/repo --attach
# detach：Ctrl-b d，或直接关终端；supervisor 继续
# 真正停止：celagent down fix-auth（flush + 最终检查点 + release lease）

# 机器 B（同一 bucket）
celagent up fix-auth --cwd ~/repo2 --restore --attach
```

B 启动时 `materialize` 从对象存储拉 manifest + segments，校验每个 segment 的 SHA-256；`--restore` 读 `workspace.latest`，下载 checkpoint/blob 恢复工作区；然后打开真 Pi 会话。

## 4. 接管（A 崩溃）

```bash
celagent status fix-auth
# lease: { owner: "host-a-1234", epoch: 3, expiresAt: ... }
```

- **同机**：owner 的 pid 已不存在 → 立即 CAS 接管，epoch+1。
- **跨机**：等 TTL 15 秒；B CAS 接管，epoch+1。
- 旧写者之后任何 manifest CAS 都会失败（fencing），不会覆盖新写者。
- lease 心跳每 5 秒续租；长检查点不会因 TTL 过期而丢写。
- 如果 A 仍活着，B 会明确报：

```text
会话 fix-auth 正被 host-a (epoch 3) 持有；先在该机器退出，或等待租约过期后接管
```

不做并发多写；跨机是顺序接管。

## 5. 工作区

```bash
# A 自动检查点；也可手动
celagent workspace checkpoint fix-auth --cwd ~/repo
celagent workspace list fix-auth

# B
celagent workspace restore fix-auth --cwd ~/repo2 --force
# 或直接
celagent up fix-auth --cwd ~/repo2 --restore
```

增量检查点：文件 SHA-256 命名 blob；首次全量，之后只传变化；默认排除 `.git`/`node_modules`/`.venv`/`dist`/`build`/`.cache`/`target`/`vendor`/`.amp`；单文件 128 MB、总量 1 GB。`--force` 只清理 checkpoint 管理路径，保留 excluded 目录。

## 6. 远程 / celld（可选）

2.0 不需要 celld。任意 Linux 主机：

```bash
ssh agent-host 'celagent up fix-auth --cwd ~/repo'
ssh -t agent-host 'celagent attach fix-auth'
```

celld 0.6.3 作为 fleet/容器后端（preview）：

```bash
./scripts/node_mgr.sh start
./scripts/node_mgr.sh status
```

- health 路径是 `/.well-known/celld/health`（0.4 起改名）。
- 不自动清理 `cells/*/own.json`；清理只能基于 `nodes/` lease 或 `celld cell gc`。
- worker 只保留 `checkpoint/resume/sync/hibernate/wake` 缓存接口；没有模拟任务。
- worker token 缺失时 fail-closed（401）；`setup.sh` 会注入 `CELLD_VAR_CELAGENT_WORKER_TOKEN`。
- 容器磁盘临时、cell 迁移会停容器：只能 checkpoint/restore，不能实时迁移。

## 7. 触发器

trigger daemon 只在一台常驻主机上启动，避免多机重复触发：

```bash
celagent trigger daemon --start
celagent trigger daemon --status
```

它启动的是新 session（`trg-<id>-<timestamp>`），不会抢用户 TUI 的 lease；同一 trigger 的 `lastPid` 存活时跳过。2.0 只支持 `--every`；cron/webhook/审批在 2.1。

## 8. 故障验证

```bash
# A 写入后直接 kill -9 TUI 进程
kill -9 <tmux pane pid>

# watchdog 2 秒内重新拉起；B 也可接管
celagent status fix-auth
celagent up fix-auth --cwd ~/repo2 --restore
```

不变量：

- 已提交 segment 不丢；
- manifest 不指向缺失 segment；
- 旧写者不能覆盖新 head；
- journal 有 start 无 finish → `unknown`，不静默重跑；
- 工作区检查点 hash 校验失败拒绝恢复。

## 9. 已知边界

- 单写者；不做并发多写。
- 跨机接管最长等 TTL 15 秒；同机 pid 检查只对同一 hostname 有效。
- 工作区检查点是异步/去抖的，不是实时同步；崩溃可能丢最后一次未检查点改动。
- 加密是整桶模式；所有机器必须用同一个 key。
- 真实 BOS/R2/S3 的长时间多机故障注入与 1 GB benchmark 仍需在有凭证的机器上跑。
- trigger preview 无审批；无人值守写操作使用独立 worktree/cwd。
