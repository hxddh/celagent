# celagent 深度评估报告

> 评估日期: 2026-08-12  
> 评估基线: `origin/main` @ `31d12a4`  
> 方法: 全量文档审阅 + 核心源码精读 + 本地语法/CLI/单测实测 + GitHub Release/CI 交叉核对  
> 目的: 对照文档声称与代码事实,给出可执行的成熟度判断与优先改进项

---

## 0. 一句话结论

**架构方向正确、工程纪律强、核心持久化路径基本立得住; 但「RPO=0 / 权威源优先恢复」的对外叙事与读路径实现不完全一致, CI 当前全红, 发布物不完整, 产品仍处「可演示的早期发布」而非「生产可依赖」。**

综合成熟度评分(满分 10):

| 维度 | 分 | 说明 |
|------|----|------|
| 产品定位与架构清晰度 | 9.0 | 三层心智模型清晰, ADR 齐全 |
| 核心持久化正确性 | 7.5 | 写路径 CAS/幂等扎实; 读路径有权威性漏洞 |
| 代码工程质量 | 7.0 | Bug 驱动修复充分; 单体文件/重复代码偏重 |
| 测试与 CI | 4.5 | 用例设计合理, 但 CI 假绿/假红并存 |
| 文档质量与一致性 | 7.0 | 文档体系优秀, 但多处已过时 |
| 发布与可安装性 | 6.0 | v0.3.0 已发, 资产缺平台, 门禁红 |
| 安全与卫生 | 8.0 | 红线明确且多轮净化; CI 扫描误伤 node_modules |
| **综合** | **7.0** | 值得继续投入的早期产品, 优先修读路径与 CI |

---

## 1. 项目定位评估

### 1.1 是什么

独立 CLI agent 产品: **不 fork** 的 Pi TUI 引擎 + Celld 分布式执行层 + BOS(S3 兼容)权威落盘。

核心卖点不是「更强的 agent 能力」,而是:

> 会话永不丢(RPO=0) — 崩溃 / 换机 / 节点故障,历史可从对象存储完整恢复。

这个定位有差异化:多数 coding agent 把会话落在本地 JSONL;celagent 把权威状态上云,并补了记忆工具与任务状态机。

### 1.2 心智模型(文档与代码一致的部分)

```
交互层: Pi TUI (库用,不 fork)
执行层: Celld 集群 (缓存 / 任务 / lease)
数据层: BOS sessions/*.json (权威源)
记忆层: history_search + session_snapshot
```

写路径双写设计合理:

1. worker 缓存 — 快、可丢、fire-and-forget(2s)
2. BOS 直写 — 慢(~0.84s)、权威、异步队列 + CAS

**评价**: ADR 十条设计决策质量高,「为什么不把 celld 当权威」有实测边界支撑(`docs/bos-compat.md` LTX ~10s 窗口、own.json 残留)。这是项目最强的工程资产。

---

## 2. 文档体系评估

### 2.1 文档地图

| 文档 | 角色 | 质量 |
|------|------|------|
| `HANDOFF.md` | 交接唯一入口 | 结构优秀; **发布状态段已过时** |
| `docs/architecture.md` | 架构权威 | 数据流/ADR/扩展点完整; §5 有陈旧断言 |
| `README.md` | 用户视角 | 命令/特性清晰 |
| `docs/bos-compat.md` | 实测边界 | 高价值,避坑指南扎实 |
| `docs/distributed-deployment.md` | 多机 | 步骤清楚;「已验证」依赖人工实测 |
| `PACKAGING.md` | 发布构建 | 匿名路径红线正确 |
| `P0-RESULT.md` | 早期 POC | 历史档案,可归档 |

### 2.2 文档 vs 现实 — 关键不一致

| # | 文档声称 | 代码/仓库事实 | 严重度 |
|---|---------|---------------|--------|
| D1 | 恢复「BOS 权威优先」「不依赖节点」(`architecture.md` §1.4, `README`) | `loadHistoryFromBos()` **先读 worker 缓存,命中即返回**,不回读 BOS | **高** |
| D2 | HANDOFF「唯一阻塞=GitHub 认证」 | 仓库已公开, `v0.3.0` Release 已存在 | 中(误导接手者) |
| D3 | `architecture.md` §5「install.sh 正式模式未切 Release」 | `install.sh` 已走 Release 下载 | 低 |
| D4 | HANDOFF 发布步骤 3「VERSION 0.1.0 vs package 0.2.0」 | 均已统一为 `0.3.0` | 低 |
| D5 | README「回归 9 用例;无节点 mock 6 pass」 | 无 `settings.json` 时 **before hook 抛 ENOENT,9 全挂** | **高** |
| D6 | 「完整记忆不截断」 | BOS 写路径不截断;但恢复若走 worker,msg 仅 200 字符 | **高**(与 D1 同源) |
| D7 | 「exactly-once」任务语义 | worker ledger 对模拟 tool 调用去重;非分布式事务语义,且依赖本机 webhook `127.0.0.1:19090` | 中(营销过载) |

**建议**: 以代码为准立刻改文档,或改代码对齐文档。D1/D6 必须二选一修齐,否则「RPO=0 完整记忆」对用户可证伪。

---

## 3. 核心代码深度评估

### 3.1 规模与结构

| 路径 | 行数 | 职责 |
|------|------|------|
| `bin/celagent-tui.mjs` | ~888 | CLI + 节点启动 + 双写 + 恢复 + TUI 编排 |
| `worker/src/index.js` | ~519 | checkpoint/任务/休眠/KV/SigV4/webhook |
| `src/bos.js` | ~145 | BOS 直写原语 |
| `src/bos-tools.js` | ~164 | 记忆工具 |
| 脚本/安装 | ~640 | install/setup/node/cluster/e2e |

总有效产品代码约 **2.3k LOC**(不含依赖)。体积小,但 **主入口过度集中** — CLI、运维、持久化、会话生命周期挤在一个文件。

### 3.2 写路径(强项)

`queueBosWrite` 体现了成熟的故障思维:

- CAS: 每次冲突重读 ETag(Bug 75)
- 首写 `If-None-Match`(Bug 76)
- 读失败绝不盲写(Bug 49)
- 队列限长 50,防堆积(Bug E)
- 退出 `await bosQueue` + 信号 flush 10s 兜底(Bug 17/48/59/60)
- 临时文件 `0600`(安全加固)

凭证策略(`awsEnv`): env 成对或 `AWS_PROFILE=bos`,禁止混用 — 与 `bos-compat.md` 实测坑一致。

**评价**: 写路径是项目技术高地,达到「可认真讨论持久化正确性」的水平。

### 3.3 读路径(关键缺陷)

```301:317:bin/celagent-tui.mjs
async function loadHistoryFromBos(sessionId) {
  // P0: 恢复读路径 — 优先 worker 缓存 (快, ~100ms), miss 回 BOS (权威, ~1.3s)
  ...
        if (data.ok && data.session && data.session.turns && data.session.turns.length > 0) {
          return data.session.turns;  // 快路径: worker 缓存命中
        }
```

而 checkpoint 写入 worker 时:

```273:273:bin/celagent-tui.mjs
const url = `...&msg=${encodeURIComponent(msg.slice(0, 200))}`;
```

**后果**:

1. 同机续写若 worker 仍热,恢复拿到的是 **截断摘要**,不是 BOS 完整 `content`/`toolResults`
2. 冷启动 sync 会把这份截断历史 **回写** worker(进一步固化)
3. 注入上下文用的 `t.msg` 因此可能残缺 — 「完整记忆」在用户可感知路径上不成立
4. 函数名 `loadHistoryFromBos` 与实现不符,增加维护误导

**修复方向**(择一):

- A(推荐,对齐文档): 恢复始终读 BOS;worker 仅作后续热读可选加速,且命中后仍需校验完整性/长度
- B: worker checkpoint 改为 POST body 存完整 turns(去掉 URL 200 限制),再允许快路径

### 3.4 记忆工具

`history_search` / `session_snapshot` 设计正确(snapshots 不碰权威 sessions)。

弱点:

- `bos-tools.js` **硬编码** `EP`,不读 `settings.persistence.endpoint`(与 `bos.js` Bug 70 不一致 → 自定义 endpoint 时记忆工具静默失败)
- `history_search` 全量 list + 逐会话下载 + 本地子串匹配 — O(会话数×体积),不可扩展
- `session_snapshot` 经 `globalThis.__celagentSnapshotTurns` 注入 — 可用但脆弱(多实例/测试污染)

### 3.5 Worker / 分布式任务

能力面宽: submit/status/ledger、hibernate/wake、kv、cwrite epoch fencing、obj-put 代理。

但:

- 任务「工具」是 payment/email/search **模拟**,依赖本机 webhook
- `exactly-once` 是单 cell ledger 去重,不是跨节点共识
- SigV4 直连路径与 webhook 代理并存;生产默认「零凭证」依赖未随包分发的 webhook 服务
- Release 含 `worker.tar.gz`,但多机文档对 webhook/部署依赖叙述偏乐观

### 3.6 依赖与运行时

- `package.json` engines: `node >= 22`
- 实际依赖 `@earendil-works/pi-*` / `undici@8` 要求 **`>=22.19.0`**
- 本评估环境 `v22.14.0` 触发 EBADENGINE 警告
- 外部硬依赖: `aws` CLI、`celld` 二进制、可选 `bun`(打包)

**评价**: 「单二进制零依赖」只覆盖 celagent 本体;真实 RPO 路径仍依赖 aws CLI + 凭证 + BOS。这对安装复杂度是诚实的,应在 README 更醒目标出。

---

## 4. 测试与 CI 评估

### 4.1 本地实测(本环境)

| 检查 | 结果 |
|------|------|
| `node --check` 全源码 | ✅ |
| `bash -n` 全部脚本 | ✅ |
| `celagent version/help` | ✅ `v0.3.0` |
| `CELAGENT_MOCK=1 npm test` | ❌ 9/9 fail — `settings.json` ENOENT 导致 before hook 失败 |
| Celld / BOS 集成 | 未测(环境无 celld/凭证) |

根因:`tests/core.test.mjs` 第二个 `before` 无条件 `readFileSync(settings.json)`,文件缺失即 hookFailed,连纯 CLI 用例也拖死。文档写的「mock 6 pass」在干净 CI/容器里不成立。

### 4.2 GitHub CI 现状

- 最近多次 `main` / PR 运行均为 **failure**
- 失败点: Secret/PII scan **扫进了 `node_modules`**,命中上游 changelog/examples 中的 `/Users/...`、`AKIA...EXAMPLE`、`aws_secret_access_key` 字符串
- 单元测试步骤带 `continue-on-error: true` — 即使修了 scan,测试仍可能「红了也绿」

### 4.3 测试缺口

- 无对 `queueBosWrite` 合并/冲突/首写的进程内单测(仅有需真实 BOS 的用例)
- 无对 `loadHistoryFromBos` worker-hit-vs-BOS 的回归(D1 盲区)
- e2e 记忆工具需真实 `DEEPSEEK_API_KEY`,不进 CI
- 无 Release 资产完整性检查(缺平台 binary 不会被测到)

---

## 5. 发布与可安装性

### 5.1 已完成

- 公开仓库 `hxddh/celagent`
- Tag / Release `v0.3.0`
- 资产: `celagent-{darwin-arm64,darwin-x64,linux-x64}`, `celld-darwin-arm64`, `install.sh`, `worker.tar.gz`
- 版本号三处统一: `package.json` / `install.sh` / CLI `0.3.0`

### 5.2 缺口

| 问题 | 影响 |
|------|------|
| 缺 `celld-linux-x64` / `celld-darwin-x64` | Linux/Intel Mac 正式安装只能回退 `celld.dev` |
| 无 Windows 资产,且 `install.sh` 直接 unsupported | 与 PACKAGING 文档不一致 |
| CI 红 | 信任度受损 |
| HANDOFF 仍写「卡认证」 | 交接信息错误 |
| 0 star / 描述为空 / topics 空 | 发现性弱(产品运营层) |

---

## 6. 「RPO=0」主张的精确边界

对外 slogan 需要收敛为可辩护表述:

| 场景 | 真实保证 |
|------|----------|
| 正常 turn_end → 队列成功落 BOS → 换机读 BOS | ✅ 历史完整 |
| 节点全挂,仅 BOS 写 | ✅ 设计支持(写不依赖节点) |
| 进程被 kill,队列未 flush 且超 10s 超时 | ⚠️ 末轮可能丢 |
| 队列 >50 丢最旧 | ⚠️ 极端高频下丢中间轮 |
| 同机热恢复走 worker 快路径 | ⚠️ **语义完整度下降**(200 字截断) |
| celld LTX 复制窗口内强杀 | ⚠️ 执行层 RestoreFailed(与会话权威无关,但影响任务/缓存) |

**建议对外口径**:「会话权威落盘在对象存储;在队列成功提交且 CAS 成功的前提下 RPO≈0。恢复应读权威源。」不要把 worker 快路径写进保证。

---

## 7. 风险矩阵(按优先级)

| 优先级 | 项 | 类型 | 建议动作 |
|--------|----|------|----------|
| P0 | 恢复读路径优先 worker 截断数据 | 正确性 | 改读 BOS,或 worker 存完整 body |
| P0 | CI Secret scan 扫 node_modules | 工程阻塞 | `--glob '!node_modules/**'` |
| P0 | 测试 before 强依赖 settings.json | 工程阻塞 | 缺失则 skip BOS 段,CLI 用例独立 |
| P1 | HANDOFF/architecture 过时断言 | 文档 | 同步发布状态与 install 现状 |
| P1 | Release 缺多平台 celld | 分发 | 补 linux/darwin-x64 或文档标明回退 |
| P1 | engines 与 undici 22.19 不对齐 | 兼容 | `engines.node: ">=22.19.0"` |
| P2 | bos-tools 硬编码 endpoint | 功能 | 复用 bos.js resolveEndpoint |
| P2 | history_search 全量扫描 | 性能 | 索引/限制扫描窗口 |
| P2 | 拆分 `celagent-tui.mjs` | 可维护性 | CLI / persist / ensureCelld 分模块 |
| P2 | 去掉测试 `continue-on-error` | CI 诚实 | mock 必过后再强制 |
| P3 | 多 provider / 快照 UI / bucket 生命周期 | 产品 | HANDOFF 已列候选 |

---

## 8. 对标「能否继续投入」的判断

### 值得保留并加大投入的部分

1. **BOS 权威 + CAS 写路径** — 差异化护城河
2. **实测驱动的 bos-compat / Bug 编号文化** — 罕见的早期项目纪律
3. **不 fork Pi** — 维护成本可控
4. **安全红线** — 发布前多轮净化,方向正确

### 需要立刻止血的部分

1. 读路径与「完整记忆」叙事对齐
2. CI 变绿且诚实(scan 排除依赖;测试不吞错)
3. 交接文档去掉「卡认证」等过期阻塞描述

### 战略判断

celagent 不是又一个「包装 LLM 的 CLI」,而是一次把 **durable agent session** 当一等公民的产品实验。技术叙事成立度约 **70%**;剩下 30% 主要在读路径一致性、CI/发布完整度、以及把「模拟任务 exactly-once」与「会话持久化」解绑表述。

若下一阶段只做三件事,应按:

1. 修恢复读路径(对齐 RPO 叙事)
2. 修 CI(scan + 无配置单测必绿)
3. 补齐 Release celld 多平台 / 刷新 HANDOFF

完成后,才适合谈多 provider、快照 UI、降本生命周期等增长项。

---

## 9. 评估证据清单

- 源码精读: `bin/celagent-tui.mjs`, `src/bos.js`, `src/bos-tools.js`, `worker/src/index.js`, `tests/core.test.mjs`, CI workflow
- 文档精读: README / HANDOFF / architecture / bos-compat / distributed / PACKAGING / P0-RESULT
- 本地命令: `node --check`, `bash -n`, `celagent version|help`, `npm test`(失败证据如上)
- GitHub: Release `v0.3.0` 资产列表; CI run `31602574447` Secret scan 失败日志(命中 `node_modules/.../Users/...`)

---

## 10. 附录:建议的文档勘误补丁清单

接手者可直接按此改文档(与代码修复可并行):

1. `HANDOFF.md` §3: 删除「卡 GitHub 认证」;改为「v0.3.0 已发布;当前阻塞=CI 红 + Release celld 多平台不全」
2. `docs/architecture.md` §1.4: 按最终代码行为重写恢复优先级;若改代码为 BOS-first,则保留原文并删 worker-first 注释
3. `docs/architecture.md` §5: 删除「install.sh 未切 Release」
4. `README.md` 测试记录: 改为「无 settings 时应 skip BOS;纯 CLI 用例独立」并与测试修复同步
5. 所有「exactly-once」旁加限定语:「单 cell execution ledger 去重」
