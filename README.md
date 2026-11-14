# celagent 1.0

**Durable agent runtime** — Pi 1.5 TUI + 分段 Pi JSONL + manifest CAS + 工具 journal + 租约。会话不是一段聊天记录，而是一个可接管、可回放、可迁移的执行日志。

```text
已提交 step 不丢
进行中的工具最多一个 unknown
有幂等键的副作用不重复
没有幂等键的副作用绝不静默重放
```

---

## 1.0 变了什么

| 维度 | 0.4.x | 1.0 |
|---|---|---|
| 持久化单位 | turn | **step**（message / tool call / tool result / compaction） |
| 写入 | 每轮整份 JSONL GET+PUT | **不可变 segment + manifest CAS** |
| 执行 | TUI 进程内的 hook | **durability extension + `celagent run` headless** |
| 并发 | 第二写者静默停写 | **lease + epoch，明确报锁 / 过期接管** |
| 工作区 | 不保存 | **`workspace snapshot/restore`** |
| 旧格式 | `.json`/steer/迁移 | **一次性 `import-v0`，运行时只认 v2** |
| 任务 | worker 模拟 step | **删除**（没有假任务） |

破坏性变更：不再读写 `sessions/<id>.json`，不再把 50 轮 steer 注入当恢复。旧会话用 `celagent import-v0 <id>` 一次性导入；旧对象保留。

---

## 安装

前置：

- Node.js **>= 22.19**
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
celagent
```

非 BOS：

```bash
celagent config set persistence.endpoint https://<account>.r2.cloudflarestorage.com
celagent config set persistence.region auto
celagent config set persistence.profile r2
celagent doctor          # CAS 探针必须通过
```

---

## 命令

| 命令 | 作用 |
|---|---|
| `celagent` | 启动 TUI（新会话） |
| `celagent <id>` | 续写 v2 会话（跨机从对象存储恢复） |
| `celagent run --prompt "..."` | 无头运行；`--session <id>`、`--json`、`--cwd` |
| `celagent list` | 列出 v2 会话；旧格式单独标注 |
| `celagent export <id>` | 导出 Pi JSONL |
| `celagent rm <id> --yes` | 删除 v2 segments + 旧对象 |
| `celagent import-v0 <id> --yes` | 旧 `.jsonl`/`.json` 一次性导入 |
| `celagent status [id]` | durable seq / segments / 租约 / outbox |
| `celagent workspace snapshot <id> [--cwd dir]` | 工作区 tar 快照 |
| `celagent workspace restore <id> [--cwd dir] [--force]` | 恢复工作区 |
| `celagent doctor` | 配置 / aws CLI / 凭证 / CAS / Pi 版本 / 可选 celld |
| `celagent cas-probe` | 条件写探测（exit 2 = 未完成，无法判定） |
| `celagent config get/set` | 配置读写 |

TUI 内：`/new`、`/fork`、`/resume`、`/compact` 仍由 Pi 提供；`/quit` 会先 flush outbox 并释放租约。

---

## 架构

```text
TUI / CLI / CI
      │  local process
      ▼
Pi 1.5 AgentSession
  └─ durability extension（before tool: journal.start；after: journal.finish）
      │
      ▼
celagent v2 engine
  ├─ lease：单写者 + epoch，过期可接管
  ├─ outbox：~/.config/celagent/outbox，先 fsync 本地再上云
  ├─ segments：按 entry 分块，不可变，hash 校验
  └─ manifest：CAS 提交 head / segments / workspace
      │
      ▼
对象存储（BOS / S3 / R2 / Tigris）
  sessions/<id>/manifest.json
  sessions/<id>/segments/000001-000005-<hash>.jsonl
  sessions/<id>/journal/<toolCallId>.json
  sessions/<id>/workspace/<hash>.tar.gz
  snapshots/<name>-<ts>.json          # pointer: session + seq
```

**权限**：会话权威仍在对象存储；celld 0.6.3 是可选执行底座（lease/alarm/container），核心不依赖它。worker 只保留可选缓存接口，不再有模拟任务。

**读路径**：`celagent <id>` → `materialize` 拉 manifest + segments → 写本地 Pi JSONL → `SessionManager.open`。每个 segment 都校验 SHA-256；manifest head 必须与最后一个 entry 一致。

**写路径**：每收到 `message_end` / `turn_end` / `session_compact`，durability engine 读本地 JSONL 的新 entry，写不可变 segment，CAS manifest，成功后清 outbox。失败留 outbox，下次启动重放。

---

## 跨机 / 接管

```bash
# 机器 A
celagent fix-auth

# 机器 B（同一 bucket）
celagent fix-auth
```

机器 A 仍持有租约时，B 会明确报：

```text
会话 fix-auth 正被 host-a (epoch 3) 持有；先在该机器退出，或等待租约过期后接管
```

A 正常退出会释放；A 崩溃时，同机 pid 已死可立即接管，跨机等 TTL（15 秒）后 epoch+1 接管。旧写者的后续 CAS 会被 fencing 拒绝。

工作区：

```bash
celagent workspace snapshot fix-auth --cwd ~/repo
# 另一台机器
celagent workspace restore fix-auth --cwd ~/repo --force
```

快照是 tar.gz，忽略 `.git`、`node_modules`、`.venv`、`dist`、`build`；单次默认上限 500 MB。容器化 workspace 在 1.1。

---

## 加密（可选）

```bash
export CELAGENT_ENCRYPTION_KEY="$(openssl rand -base64 32)"
```

设置后所有对象 body 用 AES-256-GCM 加密（manifest / segments / journal / workspace）。**key 丢失 = 数据不可读**；跨机必须使用同一 key。未设置 key 读到密文对象会明确报 `CELAGENT_ENCRYPTION_KEY`，不会把它当损坏数据。

---

## 从 0.4.x 升级

```bash
celagent import-v0 <id> --yes
```

- 优先读旧 `sessions/<id>.jsonl`，其次旧 turns JSON；
- 写成 v2 segments + manifest；
- 旧对象保留；
- 运行时不保留双读/steer 路径。

---

## 测试

```bash
npm test
```

- `tests/v2.test.mjs`：分段写/恢复、hash 校验、租约/接管、outbox、journal、加密、import、workspace。
- `tests/persist.test.mjs`：旧 `jsonlFromTurns`/谱系/尾行修复（`import-v0` 仍用）。
- `tests/core.test.mjs` / `cas-probe.test.mjs` / `p0-p2-bugs.test.mjs`：CLI、CAS 探针、存储边界。
- 需要 `celld` / 真实桶的用例无节点时 skip。

本地 S3 兼容验证（moto）：

```bash
npm test
# 另见 docs/v1.0.md 的端到端记录
```

---

## 边界

- **单写者**，不做并发多写；跨机是顺序接管。
- **不承诺任意副作用 exactly-once**。非幂等工具崩溃后标 `unknown`，需要人看 journal/工作区 diff。
- **LLM 流中断会重发**，可能重复花 token；不重复工具副作用。
- `workspace` 是 opt-in 快照，不是实时同步。
- 加密是整桶模式；同一个 bucket 不要混用加密/非加密写入。
- celld 是可选执行层；`doctor` 离线不报核心失败。
- Pi 精确钉 `1.5.2`；升级需跑完整 TUI + headless + journal 冒烟。

## 文档

- [`docs/architecture.md`](docs/architecture.md) — v1.0 架构与数据流
- [`docs/v1.0.md`](docs/v1.0.md) — 1.0 实现合同与端到端验证
- [`docs/distributed-deployment.md`](docs/distributed-deployment.md) — 多机/接管
- [`docs/s3-compat-evaluation.md`](docs/s3-compat-evaluation.md) — 合格后端与 CAS 门禁
- [`HANDOFF.md`](HANDOFF.md) — 接手入口

## License

MIT
