# celagent 架构文档

> 本文档是 celagent 的**架构权威说明**——回答"为什么这么设计"和"哪里可以改"。
> 面向:改造架构、基于迭代、新 agent 深入接手。配套:`HANDOFF.md`(交接入口)、
> `README.md`(用户视角)、`docs/distributed-deployment.md`(多机)、`docs/bos-compat.md`(BOS 边界)、
> `docs/s3-compat-evaluation.md`(多后端对象存储:合格门禁与迭代计划)、
> `docs/post-v032-evaluation.md`(v0.3.2 后排期与候选取舍)、
> `docs/post-v034-evaluation.md`(v0.3.4 后:真桶实测才是下一刀)。

## 1. 系统总览

### 1.1 三层架构(核心心智模型)

```
┌─────────────────────────────────────────────────────────────┐
│ 交互层: celagent TUI (pi-coding-agent 引擎, 不 fork)          │
│   - 对话/工具/多模型 (bash/read/write/grep/find/edit/ls 全量) │
│   - turn_end 钩子 → 双写 (worker 缓存 + BOS 直写)             │
│   - 会话恢复 (BOS 权威 → steer 注入上下文)                    │
└──────────────┬──────────────────────────────┬────────────────┘
               │ HTTP checkpoint/resume       │ aws s3api 直连
┌──────────────▼──────────────┐  ┌────────────▼─────────────────┐
│ 执行层: Celld 集群           │  │ 数据层: BOS 对象存储 (权威源) │
│   - 节点 18090/18091/19000  │  │   sessions/<id>.json 会话    │
│   - worker SQLite 缓存      │  │   cells/*/ltx 执行状态        │
│   - 任务状态机 (task)       │  │   nodes/ 集群注册 (lease)     │
│   - 休眠/唤醒/迁移          │  │   deploy/ worker 代码分发     │
└─────────────────────────────┘  └──────────────────────────────┘
```

**一句话**:对象存储保数据(权威源,RPO=0;默认 BOS)、Celld 保执行(缓存/任务/集群)、agent 可用同一存储(记忆工具)。
不是所有「S3 兼容」都能当权威源——必须有条件写 + 写后读一致;见 `docs/s3-compat-evaluation.md`。

**记忆体系**:`sessions/<id>.json`(权威会话)+ `snapshots/<name>-<ts>.json`(显式记忆锚点,
由 `session_snapshot` 工具写入,不碰权威数据,可跨会话检索 via `history_search`)。

### 1.2 节点生命周期

- **自动启动**:TUI 启动或首次写时 `ensureCelld()` 检测 18090/18091/19000 无响应
  → 从配置(bucket/凭证)拉起 celld 节点(进程内互斥锁, Bug 53 防并发重复启动)
- **运行**:worker 缓存写(2s 超时)+ BOS 直写;节点离线时自动降级(仅 BOS, 警告一次)
- **退出**:TUI 退出前 `await bosQueue` flush(Bug 17, 不丢最后几轮);节点由 nohup 托管继续运行
- **多机**:节点经 BOS `nodes/` 注册表(lease 10s)自动发现, 见 distributed-deployment.md

### 1.3 一次对话的完整旅程

```
用户输入 → pi 引擎 (LLM 推理 + 工具调用循环) → assistant 回复
   → turn_end 事件 (不阻塞对话, Bug 52: 全异步)
   → celldCheckpoint:
       ① worker 缓存: HTTP checkpoint 到任一 celld 节点
          (fire-and-forget, 2s 超时, 失败仅警告 — 缓存可重建)
       ② BOS 直写: 入异步队列 queueBosWrite (串行执行, 限长 50 —
          BOS_QUEUE_MAX, Bug E 防内存泄漏, 超出丢最旧任务)
          → bosGet 读当前对象 + ETag → 合并轮次 → If-Match CAS 写
          → 冲突(412)重读重试 ×3 → 失败警告但不阻塞对话
   → 退出前: await bosQueue (Bug 17: flush 队列, 不丢最后几轮)
```

### 1.4 恢复路径

```
celagent <id> → loadHistoryFromBos(id) (bosGet sessions/<id>.json)
   → BOS miss 时才回退 worker resume (缓存 POST JSON, msg 上限 8000; 旧 GET 兼容)
   → 取最近 50 轮 (MAX_INJECT_TURNS, Bug 78: 防超长会话撑爆模型上下文)
   → result.session.steer(...) 注入 content 文本块 (缺省回退 t.msg) + 真实 t.role
   → seq 续写起点 = max(turn) (非 turns.length, 防 gap 覆盖)
   → 运行中 message_end(user) + turn_end(assistant) 双角色落盘
优先级: BOS 权威源 > worker 缓存 (恢复先读 BOS, 不依赖节点)
```

## 2. 核心机制原理

### 2.1 为什么 BOS 是权威源(而不是 celld 状态)

- **celld 状态可丢**:节点强杀 → LTX 复制未完成 → RestoreFailed(见 bos-compat §二.6);
  own.json 残留阻塞接管(§二.8)。celld 是执行层,不是数据层。
- **BOS 直写不依赖节点**:节点全挂,对话照常持久化(实测验证:节点未启动时 41 轮全落盘)。
- **代价**:写延迟 ~0.84s(实测均值, aws CLI 签名+PUT),用**异步队列**隐藏(不阻塞对话)。

### 2.2 并发安全:三级防护

| 机制 | 防什么 | 实现 |
|---|---|---|
| If-Match 乐观锁 | 并发覆盖(多进程写同一会话) | bosGet 读 ETag → bosPut if-match → 412 冲突重读重试 |
| If-None-Match 首写 | 并发冷启动同 ID 互相覆盖丢首轮 | 仅对象不存在时写(Bug 76) |
| 单写者队列 | 进程内写序 | queueBosWrite 串行执行(BOS_QUEUE_MAX 防堆积) |
| 轮次幂等 | 续写重复 | 读 BOS 历史定 seq;同轮存在则替换(Bug 97 修复后首轮也含内容) |

### 2.3 双写一致性(worker 缓存 vs BOS)

- **写**:两路并行,worker 失败不影响 BOS(缓存丢了可重建)。
- **读**:恢复先读 BOS 权威;仅 BOS miss 时才回退 worker 缓存(checkpoint POST JSON, msg 上限 8000;
  旧 GET query 仍兼容),完整数据永远在 BOS。
- **一致性模型**:BOS 为准,缓存可过期/缺失,无强一致要求。

## 3. 组件职责边界(为什么这样划分)

### worker API 一览(Celld 节点上的完整接口, HTTP ?action=)

| action | 职责 |
|---|---|
| `checkpoint` / `resume` | 会话轮次写入 / 读取(缓存层, 双写路径用) |
| `sync` | 会话同步(节点间/与 BOS 对齐) |
| `submit` / `status` / `ledger` | 任务状态机(断点续跑, 单 cell ledger 去重) |
| `schedule` / `delegate` | 定时任务 / 跨 cell 委托 |
| `hibernate` / `wake` / `hibernate-status` | 休眠唤醒(会话即 cell, 空闲回收) |
| `kv-put/get/list/delete` | 通用 KV(缓存/协调) |
| `obj-put` / `obj-get` | 对象直读直写(webhook 代理签名, worker 零凭证) |
| `cwrite` / `cget` | 条件写/读(带 ETag 语义) |
| `webhook-test` | 连通性自检 |

**扩展任务类型**:在 worker `switch(action)` 加 case(状态机内建, 见 §3 扩展点 4)。


| 组件 | 职责 | 明确不做 | 改动入口 |
|---|---|---|---|
| `bin/celagent-tui.mjs` | CLI/交互/编排 | 不实现 LLM 协议、不实现存储细节 | 命令集、钩子、恢复策略 |
| `src/bos.js` | BOS 直写原语 | 不感知会话语义 | CAS/重试/endpoint 策略 |
| `src/bos-tools.js` | agent 记忆工具 | 不涉及交互 | 工具集扩展(新记忆工具) |
| `worker/src/index.js` | celld worker(缓存/任务/签名) | 不做权威存储 | 缓存策略、任务类型 |
| `scripts/*.sh` | 运维(节点/集群/部署) | 不进入产品代码 | 拓扑管理、部署流程 |

**扩展点(改造/迭代入口)**:
1. **换 LLM provider**:`config set provider/model` + pi 引擎支持(多模型已内建)
2. **换存储后端**:`persistence.endpoint` 白名单含 BOS / AWS S3 / R2 / Tigris host 与本机;
   其它需 `CELAGENT_ALLOW_ENDPOINT=1`。**非法 endpoint fail-closed**(不静默退回 BOS)。
   非 BOS 必须显式 `persistence.region`。BOS 为唯一实测后端;计划见
   `docs/s3-compat-evaluation.md` 与 `docs/v033-scope.md`——不要把 MinIO/B2 当 RPO=0 部署选项
3. **新记忆工具**:在 `src/bos-tools.js` 加函数,注册进 customTools 数组
4. **任务类型**:worker 的 `action=submit` switch 加分支(状态机已内建)
5. **集群拓扑**:`cluster_mgr.sh` + nodes/ 注册表(节点自动发现,无需手动 peer)
6. **恢复策略**:turn_end 钩子的注入逻辑(当前:最近 N 轮 + steer 摘要)

## 4. 关键设计决策(ADR)

| # | 决策 | 理由 | 代价 | 替代方案(为何不用) |
|---|---|---|---|---|
| 1 | **BOS 为权威源** | RPO=0、不依赖节点、换机可恢复 | 写延迟 ~0.84s | celld 状态(可丢,见 2.1) |
| 2 | **BOS 直写不走 celld** | 节点全挂数据不丢 | 双路实现复杂度 | 全走 celld(单点故障) |
| 3 | **worker 仅缓存** | 快读路径,丢了可重建 | 缓存可能过期 | 缓存做权威(违背 RPO=0) |
| 4 | **pi 引擎不 fork,库用** | 跟随上游更新,不维护 fork | 受上游 API 约束 | fork(长期维护成本) |
| 5 | **Bun 单二进制分发** | 零依赖安装,含全部依赖 | 二进制 72MB | npm 包(依赖链脆弱) |
| 6 | **GitHub Release 分发** | 天然可信渠道 + celld 随包 | 需 GitHub 认证 | npm(与零依赖定位冲突) |
| 7 | **完整记忆不截断** | 恢复上下文完整 | 对象体积增长 | 截断(丢上下文) |
| 8 | **异步队列写 BOS** | 不阻塞对话 | 退出需 flush | 同步写(卡 TUI) |
| 9 | **凭证动态获取** | 仓库零凭证,安全红线 | 用户需预配凭证 | 配置文件存凭证(泄漏面) |
| 10 | **CAS 条件写** | 并发安全 | 冲突重试开销 | 无条件覆盖(丢数据) |

## 5. 已知边界与技术债(架构视角)

- **worker 缓存上限**:checkpoint POST JSON, msg 8000 字符(旧 GET URL 兼容仍在);权威数据在 BOS,不受此限
- **LTX 异步复制窗口**:写后立即 kill 节点可能 RestoreFailed(约 10s 窗口,见 bos-compat §四)
- **own.json 残留**:强杀节点后阻塞接管,运维脚本需清理(见 setup.sh Bug 94);仅 `celagent-*` bucket 默认清理
- **节点 lease 10s**:集群成员 TTL;v0.2 对 peer 可 SIGTERM 直接 handoff,不必等 TTL。网络分区时节点仍可能被误判离线
- **celld v0.2 双监听**:Worker `127.0.0.1:18090/18091`;内部 `18092/18093`(port+2)。TUI 只打 Worker/health。详见 `docs/celld-v02-evaluation.md`
- **单写者进程内保证**:跨进程并发写靠 CAS(实测 412 拒绝,无重复无丢失);拉起节点另有 `ensure.lock`
- **CI Release job**:`.github/workflows/release.yml` 在 tag / workflow_dispatch 时匿名路径构建并上传
- **Release 资产**:v0.3.4 含 celagent 五平台、celld linux/darwin-arm64、SHA256SUMS;上游 celld 无 darwin-x64/Windows
- **存储后端**:运维脚本从 settings 读 endpoint/region/profile;`resolveEndpoint` 对非白名单 URL **fail-closed**。`celagent doctor` / `cas-probe` 验证 If-Match 真的执行。CAS 实测非 BOS 与「已支持 R2」见 v0.3.5 / `docs/s3-compat-evaluation.md`

## 6. 与分布式部署的关系

- 单机 = 双节点(18090/18091);多机 = cluster_mgr 加节点(见 distributed-deployment.md)
- 多机共享同一 bucket → 会话跨机器可见(BOS 权威)
- 节点经 BOS `nodes/` 注册表自动发现,无手动 peer 配置
- 任务状态机 submit/status/ledger 跨节点断点续跑(单 cell ledger 去重, 不是跨节点共识)
