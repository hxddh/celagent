# celagent 1.0 架构

> 权威说明：为什么这么设计、数据怎么流、哪里可以改。
> 配套：`README.md`（用户）、`docs/v1.0.md`（实现合同/验证）、`HANDOFF.md`（接手）。

## 1. 一句话

**会话是一个可接管的执行日志：Pi 负责语义，celagent 负责把每个 step 变成对象存储里的不可变对象。**

```
交互层  TUI / CLI / CI
          │
执行层  Pi 1.5 AgentSession + durability extension
          │  tool_call → journal.start；tool_result → journal.finish
          │  message_end / turn_end / session_compact → segment append
          ▼
持久层  celagent v2 engine
          ├─ lease.json（单写者 + epoch）
          ├─ outbox（本地 fsync）
          ├─ segments（不可变，hash 校验）
          └─ manifest.json（CAS head/segments/workspace）
          │
数据层  对象存储（BOS / S3 / R2 / Tigris）
```

celld 0.6.3 是**可选执行底座**（lease/alarm/container/OTEL）；核心会话路径不依赖它。worker 只保留可选缓存接口，不再有模拟任务。

## 2. 为什么保留 Pi JSONL

- Pi 已经处理分支、compaction、context edit、tool call/result 的树结构；重写等于 fork 会话格式。
- JSONL 是追加式日志；按 entry id 分块天然幂等。
- `SessionManager.open` 能直接打开物化后的 JSONL；跨机恢复不是「50 轮作文」。

**分段的是 JSONL 的 entry 流，不是另造一套事件模型。**

## 3. 一次写入

```
用户消息 / 工具结果 / compaction
   → Pi 写本地 JSONL
   → celagent 读新 entry（按 manifest.head 做前缀校验）
   → 写 segment（If-None-Match: *）
   → CAS manifest（If-Match）
   → 清 outbox
```

失败：

- 写 segment 失败 → 留 outbox，下次重试；
- manifest CAS 冲突 → 重读，前缀一致则重试；分叉 → `LineageMismatchError`；
- segment 已写、manifest 未提交 → 孤儿 segment，GC 兜底；
- 进程退出 → 本地 outbox 重放；`/quit`/SIGINT/SIGTERM 都先 flush 再释放 lease。

## 4. 一次恢复

```
celagent <id>
  → lease.acquire
  → materialize: 读 manifest + segments，校验每个 hash
  → 写本地 <id>.jsonl
  → SessionManager.open
  → 有 outbox 尾部则 appendFromFile 补写
```

本地领先（崩溃前已写本地、BOS 未落地）时，materialize 保留本地并 `needsAppend=true`；本地落后/缺失才用远端覆盖；谱系不同源直接拒绝。

## 5. 工具 journal

`tool_call` 是 Pi 1.5 的可阻塞扩展点：

- `journal.start` 先写本地 pending（fsync），再写远端；远端失败 → block 工具（fail-closed）。
- `journal.finish` 写结果；远端失败留本地，下次 `reconcile` 标 `unknown`。
- 恢复时按 `toolCallId` 对账：有 start 有 finish → 跳过；有 start 无 finish → `unknown`；有 finish 无 start → 不应存在。

这是「执行可恢复」与「会话备份」的分界线：**意图先于副作用落盘**。

## 6. 租约

- `sessions/<id>/lease.json`：`{owner, epoch, expiresAt}`。
- 单写者；第二写者得到 `SessionLockedError`，不是静默停写。
- 同机 pid 已死 → 立即 CAS 接管；跨机等 TTL 15 秒；epoch+1。
- 每次 append 前 `lease.assert`；旧写者的 manifest CAS 失败（fencing）。
- 不做并发多写；跨机是顺序接管。

## 7. 工作区

- `workspace snapshot`：tar.gz 当前目录，忽略 `.git`/`node_modules`/`.venv`/`dist`/`build`；上限 500 MB。
- `workspace restore`：解包到 cwd；非空需 `--force`。
- manifest 记录 latest snapshot；不是实时同步。容器化 workspace 是 1.1。

## 8. 记忆

- `history_search`：读 v2 manifest + segments，返回 `session/seq/role/snippet`；默认当前会话，`*` 跨会话（最多 20 个）。
- `session_snapshot`：只写 pointer `{session, seq, entryId}`，不复制 turns。
- 1.1 计划加 FTS 索引；1.0 仍是按需读 segment。

## 9. 加密

- `CELAGENT_ENCRYPTION_KEY` = 32 字节 base64；AES-256-GCM 信封加密所有对象 body。
- manifest 也加密；没有 key 的客户端得到 `encryption-key-required`，不会把密文当损坏。
- key 丢失 = 数据不可读；同一 bucket 不混用加密/非加密。

## 10. 边界

- 任意副作用 exactly-once 不可能；只承诺「有幂等键不重复；没有幂等键不静默重放」。
- LLM 流中断会重发；可能重复花 token，不重复工具副作用。
- 单写者，不做并发多写。
- celld 可选；核心不依赖节点。
- Pi 精确钉 1.5.2；升级需跑完整 TUI + headless + journal 冒烟。

## 11. 扩展点

| 想改 | 位置 |
|---|---|
| 换存储后端 | `src/v2/adapter.js` + `src/bos.js`；合同测试 `probeStoreCas` |
| 改租约 TTL/接管 | `src/v2/lease.js` |
| 改分段大小 | `src/v2/session.js` 的 `SEGMENT_MAX_*` |
| 改 journal 语义 | `src/v2/journal.js` + `src/v2/extension.js` |
| 加 workspace 后端 | `src/v2/workspace.js` |
| 加 headless 模式 | `bin/celagent-tui.mjs` 的 `runHeadless` |
| 加 celld 执行 | `worker/` + celld 0.6.3 的 container/Workflow |

## 12. ADR

| # | 决策 | 理由 | 代价 |
|---|---|---|---|
| 1 | 对象存储是会话权威 | 节点可丢，会话不能丢 | 写延迟；需要 CAS |
| 2 | 保留 Pi JSONL 语义 | 不 fork 会话格式；`SessionManager.open` 直接可用 | 需要分段/物化 |
| 3 | segment 不可变 + manifest CAS | 追加 O(1)，并发可检测 | manifest 单点 CAS |
| 4 | 单写者 lease | 编码 agent 不需要并发编辑 | 跨机要等 TTL/接管 |
| 5 | 工具 journal fail-closed | 副作用不能在没有 durable intent 时执行 | 存储不可用时工具被 block |
| 6 | 不做任意副作用 exactly-once | 分布式系统做不到；诚实比口号重要 | 非幂等工具会 `unknown` |
| 7 | 工作区 opt-in 快照 | 每轮 tar 成本高 | 崩溃时可能丢最后工作区 |
| 8 | 加密整桶 | 简单、可验证 | 不能混用加密/非加密 |
| 9 | celld 可选 | 核心不依赖节点 | 少一个卖点，但更可靠 |
| 10 | Pi 精确钉版本 | 扩展/事件 API 仍在演进 | 升级要跑冒烟 |
