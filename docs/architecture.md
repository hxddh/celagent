# celagent 架构文档

> 本文档是 celagent 的**架构权威说明**——回答"为什么这么设计"和"哪里可以改"。
> 面向:改造架构、基于迭代、新 agent 深入接手。配套:`HANDOFF.md`(交接入口)、
> `README.md`(用户视角)、`docs/distributed-deployment.md`(多机)、`docs/bos-compat.md`(BOS 边界)。

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

**一句话**:BOS 保数据(权威源,RPO=0)、Celld 保执行(缓存/任务/集群)、agent 可用 BOS(记忆工具)。

### 1.2 一次对话的完整旅程

```
用户输入 → pi 引擎 (LLM 推理 + 工具调用循环) → assistant 回复
   → turn_end 事件 (不阻塞对话, Bug 52: 全异步)
   → celldCheckpoint:
       ① worker 缓存: HTTP checkpoint 到任一 celld 节点
          (fire-and-forget, 2s 超时, 失败仅警告 — 缓存可重建)
       ② BOS 直写: 入异步队列 queueBosWrite (串行执行)
          → bosGet 读当前对象 + ETag → 合并轮次 → If-Match CAS 写
          → 冲突(412)重读重试 ×3 → 失败警告但不阻塞对话
   → 退出前: await bosQueue (Bug 17: flush 队列, 不丢最后几轮)
```

### 1.3 恢复路径

```
celagent <id> → loadHistoryFromBos(id) (bosGet sessions/<id>.json)
   → 取最近 N 轮 → result.session.steer("以下是本会话之前的对话历史...")
   → seq 续写起点 = BOS 历史长度 (防二次 resume 覆盖旧轮)
优先级: BOS 权威源 > worker 缓存 (恢复读 BOS, 不依赖节点)
```

## 2. 核心机制原理

### 2.1 为什么 BOS 是权威源(而不是 celld 状态)

- **celld 状态可丢**:节点强杀 → LTX 复制未完成 → RestoreFailed(见 bos-compat §二.6);
  own.json 残留阻塞接管(§二.8)。celld 是执行层,不是数据层。
- **BOS 直写不依赖节点**:节点全挂,对话照常持久化(实测验证:节点未启动时 41 轮全落盘)。
- **代价**:写延迟 ~0.8s(aws CLI 签名+PUT),用**异步队列**隐藏(不阻塞对话)。

### 2.2 并发安全:三级防护

| 机制 | 防什么 | 实现 |
|---|---|---|
| If-Match 乐观锁 | 并发覆盖(多进程写同一会话) | bosGet 读 ETag → bosPut if-match → 412 冲突重读重试 |
| If-None-Match 首写 | 并发冷启动同 ID 互相覆盖丢首轮 | 仅对象不存在时写(Bug 76) |
| 单写者队列 | 进程内写序 | queueBosWrite 串行执行(BOS_QUEUE_MAX 防堆积) |
| 轮次幂等 | 续写重复 | 读 BOS 历史定 seq;同轮存在则替换(Bug 97 修复后首轮也含内容) |

### 2.3 双写一致性(worker 缓存 vs BOS)

- **写**:两路并行,worker 失败不影响 BOS(缓存丢了可重建)。
- **读**:恢复只读 BOS 权威;worker 缓存是快路径(148ms),截断 200 字符(URL 限制),
  完整数据永远在 BOS。
- **一致性模型**:BOS 为准,缓存可过期/缺失,无强一致要求。

## 3. 组件职责边界(为什么这样划分)

| 组件 | 职责 | 明确不做 | 改动入口 |
|---|---|---|---|
| `bin/celagent-tui.mjs` | CLI/交互/编排 | 不实现 LLM 协议、不实现存储细节 | 命令集、钩子、恢复策略 |
| `src/bos.js` | BOS 直写原语 | 不感知会话语义 | CAS/重试/endpoint 策略 |
| `src/bos-tools.js` | agent 记忆工具 | 不涉及交互 | 工具集扩展(新记忆工具) |
| `worker/src/index.js` | celld worker(缓存/任务/签名) | 不做权威存储 | 缓存策略、任务类型 |
| `scripts/*.sh` | 运维(节点/集群/部署) | 不进入产品代码 | 拓扑管理、部署流程 |

**扩展点(改造/迭代入口)**:
1. **换 LLM provider**:`config set provider/model` + pi 引擎支持(多模型已内建)
2. **换存储后端**:`settings.json` 的 `persistence.endpoint` 指向任意 S3 兼容服务
   (自定义 endpoint 透传设计, Bug 70;底层是 `src/bos.js` 的 aws CLI 封装,
   BOS 为本项目唯一实测后端, 其他 S3 兼容服务需自行验证)
3. **新记忆工具**:在 `src/bos-tools.js` 加函数,注册进 customTools 数组
4. **任务类型**:worker 的 `action=submit` switch 加分支(状态机已内建)
5. **集群拓扑**:`cluster_mgr.sh` + nodes/ 注册表(节点自动发现,无需手动 peer)
6. **恢复策略**:turn_end 钩子的注入逻辑(当前:最近 N 轮 + steer 摘要)

## 4. 关键设计决策(ADR)

| # | 决策 | 理由 | 代价 | 替代方案(为何不用) |
|---|---|---|---|---|
| 1 | **BOS 为权威源** | RPO=0、不依赖节点、换机可恢复 | 写延迟 ~0.8s | celld 状态(可丢,见 2.1) |
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

- **worker 200 字符截断**:URL 长度限制,缓存层妥协,权威数据不受影响
- **LTX 异步复制窗口**:写后立即 kill 节点可能 RestoreFailed(约 10s 窗口,见 bos-compat §四)
- **own.json 残留**:强杀节点后阻塞接管,运维脚本需清理(见 setup.sh Bug 94)
- **节点 lease 10s**:集群成员 TTL,网络分区时节点可能被误判离线
- **单写者进程内保证**:跨进程并发写靠 CAS(实测 412 拒绝,无重复无丢失)
- **CI 不构建发布二进制**:当前手动匿名路径构建(见 PACKAGING 注意 0)
- **install.sh 正式模式未切 Release 下载**:发布流程步骤 5

## 6. 与分布式部署的关系

- 单机 = 双节点(18090/18091);多机 = cluster_mgr 加节点(见 distributed-deployment.md)
- 多机共享同一 bucket → 会话跨机器可见(BOS 权威)
- 节点经 BOS `nodes/` 注册表自动发现,无手动 peer 配置
- 任务状态机 submit/status/ledger 跨节点断点续跑(exactly-once)
