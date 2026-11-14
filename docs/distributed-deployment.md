# celagent 1.0 多机部署与接管

> 目标：同一 bucket，任何机器都能续写；单写者租约，第二台明确报锁，过期后 epoch+1 接管。
> 会话权威是对象存储里的 v2 manifest + segments；celld 是可选的执行底座，不是数据层。

## 1. 架构

```
机器 A / B / C
  celagent TUI 或 run
      │  v2 engine
      ▼
对象存储（同一 bucket）
  sessions/<id>/manifest.json
  sessions/<id>/segments/*.jsonl
  sessions/<id>/journal/*.json
  sessions/<id>/lease.json
  sessions/<id>/workspace/*.tar.gz

可选：celld 0.6.3 节点（18090/18091）
  worker 只做本地缓存；会话读写不经过它
```

## 2. 每台机器

```bash
# Node >= 22.19 + aws CLI
CELAGENT_SRC=~/celagent ./install.sh

# 同一份 settings（bucket/endpoint/region/profile）
cat ~/.config/celagent/settings.json

celagent doctor
# 必须看到 [5/6] CAS ✓；aws CLI ✓
```

`persistence.bucket` 必须相同；`endpoint`/`region`/`profile` 也必须一致。

## 3. 正常交接

```bash
# 机器 A
celagent fix-auth
# 退出（/quit、Ctrl+C、SIGTERM 都会先 flush outbox 并释放 lease）

# 机器 B
celagent fix-auth
```

B 启动时 `materialize` 从对象存储拉 manifest + segments，校验每个 segment 的 SHA-256，然后打开真 Pi 会话。

## 4. 接管（A 崩溃）

```bash
celagent status fix-auth
# lease: { owner: "host-a-1234", epoch: 3, expiresAt: ... }
```

- **同机**：owner 的 pid 已不存在 → B 立即 CAS 接管，epoch+1。
- **跨机**：等 TTL 15 秒；B CAS 接管，epoch+1。
- 旧写者之后任何 manifest CAS 都会失败（fencing），不会覆盖新写者。
- 如果 A 仍活着，B 会明确报：

```text
会话 fix-auth 正被 host-a (epoch 3) 持有；先在该机器退出，或等待租约过期后接管
```

不做并发多写；跨机是顺序接管。

## 5. 工作区

```bash
# A
celagent workspace snapshot fix-auth --cwd ~/repo

# B
celagent workspace restore fix-auth --cwd ~/repo --force
```

快照是 tar.gz（忽略 `.git`/`node_modules`/`.venv`/`dist`/`build`；默认 500 MB 上限）。

## 6. celld（可选）

1.0 核心不依赖 celld；需要节点缓存/未来的容器 workspace 时：

```bash
# celld 0.6.3 随包或单独安装
./scripts/node_mgr.sh start
./scripts/node_mgr.sh status
```

- health 路径是 `/.well-known/celld/health`（0.4 起改名）。
- 1.0 不再自动清理 `cells/*/own.json`；清理只能基于 `nodes/` lease 或 `celld cell gc`。
- worker 只保留 `checkpoint/resume/sync/hibernate/wake` 缓存接口；没有模拟任务。
- worker token 缺失时 fail-closed（401）；`setup.sh` 会注入 `CELLD_VAR_CELAGENT_WORKER_TOKEN`。

## 7. 故障验证

```bash
# A 写入后直接 kill（模拟崩溃）
kill -9 <celagent-pid>

# B
celagent status fix-auth        # 看 lease
celagent fix-auth               # 同机 pid 死立即接管；跨机等 TTL
```

不变量：

- 已提交 segment 不丢；
- manifest 不指向缺失 segment；
- 旧写者不能覆盖新 head；
- journal 有 start 无 finish → `unknown`，不静默重跑。

## 8. 已知边界

- 单写者；不做并发多写。
- 跨机接管最长等 TTL 15 秒；同机 pid 检查只对同一 hostname 有效。
- 工作区快照是 opt-in，不是实时同步。
- 加密是整桶模式；所有机器必须用同一个 `CELAGENT_ENCRYPTION_KEY`。
- 真实 BOS/R2/S3 的长时间多机故障注入仍需在有凭证的机器上跑。
