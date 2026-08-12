# celagent 深度评估报告

> 评估日期: 2026-08-12(三轮)  
> 评估基线: `origin/main` @ `31d12a4`(+ 对照 PR#1 `cursor/security-sanitize-2d82`)  
> 方法: 全量文档审阅 + 核心源码精读 + 并发/安全专项审查 + 本地实测 + GitHub Release/CI/开源 PR 交叉核对  
> 目的: 对照文档声称与代码事实,给出可执行的成熟度判断与优先改进项

---

## 0. 一句话结论

**架构方向正确、工程纪律强、写路径 CAS 扎实; 但读路径/队列丢弃/ensureLock/仅 assistant 落盘等会实质性削弱「RPO=0」, CI 与 Release 未闭环, 安全面仍有本地威胁模型缺口。综合仍属「可演示的早期发布」, 合入 PR#1 并修 P0 正确性后才能谈生产可信。**

综合成熟度评分(满分 10, **第三轮校准**):

| 维度 | 分 | 说明 |
|------|----|------|
| 产品定位与架构清晰度 | 9.0 | 三层心智模型清晰, ADR 齐全 |
| 核心持久化正确性 | **6.5** | 写路径强; 读/队列/sync/seq 有可证伪丢数据路径 |
| 代码工程质量 | 7.0 | Bug 驱动充分; 单体文件/重复 `awsEnv`/静默 catch 偏多 |
| 测试与 CI | **4.0** | 设计合理; scan 误伤 + 无配置挂死 + continue-on-error |
| 文档质量与一致性 | 7.5 | 体系优秀; 本评估 PR 已修过时阻塞; 语义口径仍偏乐观 |
| 发布与可安装性 | **5.5** | 缺 celld-linux/windows; Release install ≠ main |
| 安全与卫生 | **7.0** | 红线好; 无校验下载、/tmp、无鉴权 worker、凭证读入堆 |
| **综合** | **6.7** | 四轮排查后叙事收紧;先修正确性+合入安全 PR |

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

1. `HANDOFF.md` §3: 删除「卡 GitHub 认证」;改为「v0.3.0 已发布;当前阻塞=CI 红 + Release celld 多平台不全」 — **本 PR 已改**
2. `docs/architecture.md` §1.4: 按最终代码行为重写恢复优先级;若改代码为 BOS-first,则保留原文并删 worker-first 注释
3. `docs/architecture.md` §5: 删除「install.sh 未切 Release」 — **本 PR 已改**
4. `README.md` 测试记录: 改为「无 settings 时应 skip BOS;纯 CLI 用例独立」并与测试修复同步
5. 所有「exactly-once」旁加限定语:「单 cell execution ledger 去重」

---

## 11. 第二轮深度审查(2026-08-12 续)

> 范围:安装/运维脚本、doctor 自检、凭证路径一致性、Release 资产实测、
> `cursor/security-sanitize-2d82` 分支对照、TUI 冷启动实测。

### 11.1 新发现(按严重度)

| ID | 发现 | 证据 | 严重度 |
|----|------|------|--------|
| R2-1 | **`doctor` 假阴性**:检查 `models.json`,但 pi 0.84 实际写 `models-store.json`;缺 `models.json` 时 TUI 仍可启动,doctor 却报「TUI 无法启动」 | 冷启动实测:services/会话就绪;`pi-runtime/` 仅有 `settings.json`/`auth.json`/`models-store.json`;`doctor` 输出 `models.json 缺缺失` | **高**(误导排障) |
| R2-2 | **持久化只存 assistant 轮**,不存 user 消息;`turn_end` 硬编码 `role: "assistant"` | `bin/celagent-tui.mjs` ~832 行;恢复注入也只拼 assistant `msg` | **高**(「完整会话」名不副实;跨机续写缺用户侧上下文) |
| R2-3 | **`main` 落后安全加固分支**:`cursor/security-sanitize-2d82` ahead_by=1,含 install 私有临时目录、测试 ENOENT 容错等 | `gh compare main...cursor/security-sanitize-2d82`;Release 上的 `install.sh` 比当前树更严(先 unset AK/SK、`mktemp -d` 700) | **高**(发布物与源码漂移) |
| R2-4 | **`celld-linux-x64` Release 404** | `curl` 跟随跳转后 HTTP 404;资产列表仅有 `celld-darwin-arm64` | **高**(Linux 正式安装必回退第三方) |
| R2-5 | CLI 的 `list`/`export`/`rm`/`doctor` BOS 探测 **强制 `AWS_PROFILE=bos`**,无视已成对的 env 凭证 | 与 `src/bos.js` `awsEnv()`「env 优先」策略分裂 | 中 |
| R2-6 | 全局 `bosWarned` 一把梭:任意一类警告置位后,**后续不同类警告全部静默** | 队列过长 / worker 失败 / 读失败 / 写失败共用一个 flag | 中 |
| R2-7 | `install.sh`/`setup.sh` **整文件覆盖** `settings.json`,重装会抹掉手改的 provider/model 等 | heredoc `cat > settings.json` | 中 |
| R2-8 | `cluster_mgr.sh start` **不清理 own.json**,而 `node_mgr.sh`/`ensureCelld`/`setup.sh` 会 — 多机入口行为不一致 | 脚本对照 | 中 |
| R2-9 | `pkill -f 'celld.*1809'` 过宽,可能误杀无关进程 | install/setup/node_mgr/cluster_mgr | 中 |
| R2-10 | `listSessions` 巨型硬编码 denylist 过滤测试会话前缀 — 易漏/易误伤真实会话名 | ~391 行超长正则 | 低–中 |
| R2-11 | `ensureCelld` 启动前用 **`execFileSync` 清 own.json**,阻塞事件循环 | ~85–106 行;与 Bug 59「禁同步 aws」精神冲突 | 低–中 |
| R2-12 | security 分支修了测试 ENOENT,但 **CI scan 仍未排除 `node_modules`**,且去掉 `continue-on-error` 后会更红 | 分支 ci.yml 仍无 `!node_modules/**` | 中(合并前必修) |

### 11.2 安装体验实测结论

- 开发态 `node bin/celagent-tui.mjs`:无 settings / 无凭证时,**TUI 仍能起来**(降级运行),与 doctor「无法启动」矛盾。
- pi 引擎会自动在 `~/.config/celagent/pi-runtime/` 落盘最小配置;`install.sh` 不引导 pi-runtime **本身可接受**,但 doctor 检查项必须与 pi 0.84 文件名对齐。
- Release `celagent-linux-x64`(~102MB)可下载;`strings` 未命中本轮红线路径模式(抽样通过)。
- Release `install.sh` ≠ 当前 `main`/`本分支` 的 `install.sh`(安全卫生细节 Release 更新)→ **需要把安全分支合入 main 并重发 install.sh 资产**,否则「curl\|sh 最新」与仓库源码分叉。

### 11.3 会话语义再澄清

当前产品实际保证更接近:

> **assistant 轮次的权威落盘 + 摘要级恢复注入**(最近 50 轮 `msg`),不是双向完整对话 transcript。

若产品坚持「会话永不丢」,应明确:

1. 是否要持久化 user 轮(推荐:是,否则跨机续写只能靠本地 JSONL)
2. 恢复时是否应优先用 BOS 的 `content`/`toolResults` 重建,而非截断 `msg`
3. `/resume` 本地 JSONL 与 BOS 镜像的权威关系(文档已暗示本地可更完整 — 需写成显式契约)

### 11.4 更新后的优先队列(覆盖 §7)

| 优先级 | 项 | 动作 |
|--------|----|------|
| P0 | 合入 `security-sanitize` + CI 排除 `node_modules` | 消除源码/Release 漂移与假红 |
| P0 | 恢复读路径 BOS-first 或完整 body | 对齐 RPO/完整记忆 |
| P0 | 修 doctor:`models-store.json` 或改为「可启动/降级」语义 | 去掉假阴性 |
| P0 | 明确并实现 user 轮持久化(或改文档降级主张) | 会话语义诚实 |
| P1 | 补 `celld-linux-x64`(及 darwin-x64)到 Release | 正式安装闭环 |
| P1 | CLI 路径统一 `awsEnv()` | 凭证策略一致 |
| P1 | 拆分 `bosWarned` / settings 合并写 / cluster own.json | 运维一致性 |
| P2 | 其余 §7 P2–P3 项 | 可维护性与产品化 |

### 11.5 评分微调

第二轮后,**综合分维持 7.0**,但分项调整:

| 维度 | 首轮 | 次轮 | 变化原因 |
|------|------|------|----------|
| 核心持久化正确性 | 7.5 | **7.0** | 确认仅 assistant 落盘 + 读路径截断叠加 |
| 测试与 CI | 4.5 | **4.0** | 安全分支亦未修 scan;doctor 与测试双假 |
| 发布与可安装性 | 6.0 | **5.5** | celld-linux 404 + Release/源码 install 漂移 |
| 文档一致性 | 7.0 | **7.5** | 本 PR 已修正过时阻塞描述 |

**结论不变**:值得继续投入;下一迭代必须先做「语义诚实 + CI/发布闭环」,再谈功能扩展。

---

## 12. 第三轮全面排查(并发 + 安全 + 依赖 + Release 矩阵)

> 范围:并发/一致性专项、安全威胁模型、`npm audit`、Release 全资产 HTTP 矩阵、
> 开源 PR 对照(PR#1 安全加固 vs main)、HTML 演示页抽样。  
> 方法:源码精读 + 两路并行专项审查 + `gh`/`curl`/`npm` 实测。

### 12.1 Release 资产矩阵(实测 HTTP)

| 资产 | HTTP | 备注 |
|------|------|------|
| celagent-darwin-arm64 / x64 / linux-x64 | 200 | OK |
| celagent-windows-x64.exe | **404** | PACKAGING 写了 Windows,未发 |
| celld-darwin-arm64 | 200 | OK |
| celld-darwin-x64 / celld-linux-x64 | **404** | Linux 安装必回退 celld.dev |
| install.sh / worker.tar.gz | 200 | OK;install 内容 ≠ 当前 main |

### 12.2 并发 / 数据丢失 — 新 Critical 发现

| ID | 严重度 | 问题 | 场景后果 | 修复方向 |
|----|--------|------|----------|----------|
| R3-C1 | **Critical** | worker 截断缓存命中后**永不对照 BOS**;续写用 `seq=length` + 同 turn **替换** | 热节点续写可能用截断内容**覆盖** BOS 完整轮 | 恢复 BOS-first;或 worker 命中后仍 GET BOS 取较新/较完整副本;禁止无条件 replace |
| R3-C2 | **Critical** | 启动 `sync` fire-and-forget **盲写**旧 `savedHistory` | 用户已产生新 turn 后 sync 落地 → **抹掉** worker 新轮 | sync 按 turn 合并 / 带 generation / 接受输入前 await sync |
| R3-C3 | **Critical** | `BOS_QUEUE_MAX` 超限时 `return` — **丢掉的是最新写入**(注释写「丢最旧」) | 高频对话静默丢最近轮;flush 无法挽回 | 丢最旧或本地 WAL 背压;禁止丢最新 |
| R3-H1 | **High** | `ensureCelld` 健康节点 early-return **不释放 `ensureLock`** | Celld 中途挂掉后自动拉起永久失效 | `run()` 顶层 `finally` 清锁 |
| R3-H2 | **High** | `bosGet`:先 get body 再 head ETag → **TOCTOU** | CAS 基线错位 → 冲突耗尽丢轮或合并错版本 | GET 与 ETag 同响应获取 |
| R3-H3 | **High** | ledger:`webhook` 先于 `put` ledger | 崩溃后重放 → 副作用重复(非 crash-safe exactly-once) | 先写 pending 再副作用 |
| R3-H4 | **High** | 信号 flush 与 pi `exit(0)` 竞态 | 队列未排空即杀进程 → 末轮丢 | 接入 pi dispose 钩子或本地 WAL |
| R3-M1 | Medium | `seq` 用 `turns.length` 而非 `max(turn)` | 有 gap/remap 时误替换旧轮 | `seq = max(turn)\\|0` |
| R3-M2 | Medium | worker 单 `setAlarm` 槽:task 覆盖 cron | 定时任务停摆 | `min(所有唤醒点)` 一次 set |
| R3-M3 | Medium | `globalThis.__celagentSnapshotTurns` 返回活数组 | 序列化中途被 turn_end 突变;/new 切换会话串快照 | 返回 slice 拷贝;按 persistId 隔离 |
| R3-M4 | Medium | `cwrite` 锁无 TTL | isolate 崩溃后永久 writer-busy | 锁带过期时间 |
| R3-M5 | Medium | 跨进程 ensureCelld probe→spawn TOCTOU | 双启动争端口/ownership 抖动 | 状态目录文件锁 |

### 12.3 安全威胁模型 — 发现与 PR#1 对照

| ID | 严重度 | 问题 | main 现状 | PR#1(`security-sanitize`) |
|----|--------|------|-----------|---------------------------|
| R3-S1 | **Critical** | `curl\|sh` + 无校验和二进制 + 无路径守卫 untar + 未固定 `npx esbuild` | 存在 | 部分(私有 tmp);**仍无 checksum/签名** |
| R3-S2 | **High** | `bos-tools` 可预测 `/tmp/celagent-*` 名(symlink 覆盖) | 存在 | **已修**(mkdtemp) |
| R3-S3 | **High** | worker agent API **无鉴权**(本机任意进程可 resume/sync/submit/webhook-test) | 存在 | 未加 auth;webhook 可配置+默认 loopback |
| R3-S4 | **High** | `ensureCelld` **`readFileSync(~/.aws/credentials)`** 把 SK 读进堆 | 存在 | 需确认是否改掉(对照:应改用 `aws configure get`) |
| R3-S5 | Medium | checkpoint `msg` 进 **URL query**(日志泄漏对话片段) | 存在 | 仍在 |
| R3-S6 | Medium | `persistence.endpoint` 无白名单 → 会话 JSON 可被导向恶意 endpoint | 存在 | 仍在 |
| R3-S7 | Medium | `projectTrusted: true` 默认信任 cwd 项目配置 | 存在 | 仍在 |
| R3-S8 | Medium | `history_search` 默认可跨会话检索 → prompt 诱导泄密 | 存在 | 仍在 |
| R3-S9 | Medium | 启动时**清空 bucket 全部 `own.json`** | 存在 | 仍在(共享 bucket 危险) |
| R3-S10 | Low–Med | settings 写入无 `0600` | 存在 | PR#1 声称 settings 0600 |
| R3-S11 | Low | `list` 无配置时枚举账号全部 bucket | 存在 | 仍在 |

**已做得好的**(应保留):`execFile` 数组传参防注入;脚本 `env -u` + profile 不混用;`bos.js` randomBytes+0600;celld 默认 loopback;BOS CAS;CI 有 secret 模式扫描;bucket 名随机无 whoami。

### 12.4 依赖与运行时

- `npm audit --omit=dev`: **0 vulnerabilities**(抽样时点)
- `package.json` engines `>=22` **低于** `@earendil-works/pi-*` / `undici@8` 要求的 `>=22.19.0`(本环境 22.14 已 EBADENGINE)
- HTML 演示页抽样:会话 ID 已脱敏为 `sess-demo-xxxxxxxx`,未见明文密钥路径

### 12.5 开源协作状态

| PR | 分支 | 作用 | 与本评估关系 |
|----|------|------|--------------|
| #1 | `cursor/security-sanitize-2d82` | awsEnv 统一、mkdtemp、session ID 白名单、CI 去 continue-on-error、worker webhook 硬化 | **应优先合入**;合入前补 `!node_modules/**` 否则仍红 |
| #2 | `cursor/project-deep-eval-0737`(本 PR) | 三轮评估文档 + HANDOFF/architecture 同步 | 文档/决策输入,不改运行时 |

### 12.6 「RPO=0」主张 — 第三轮后的精确表述(建议对外改写)

**当前代码可辩护的保证**:

> 在 BOS 写成功入队且 CAS 提交成功、且恢复走 BOS 全量对象的前提下,assistant 轮次可跨机器恢复。

**当前代码不能声称的**:

- 双向完整对话永不丢(缺 user 轮)
- 任意崩溃 RPO=0(队列丢最新、信号竞态、flush 超时)
- worker 热恢复等同完整记忆(200 字截断 + 可覆盖权威)
- 任务侧 crash-safe exactly-once(ledger 写在副作用之后)

### 12.7 统一优先队列(三轮合并,按执行顺序)

#### 立即(阻塞可信发布)

1. **合入 PR#1** + CI 增加 `--glob '!node_modules/**'`(及 grep `--exclude-dir=node_modules`)
2. **恢复路径 BOS-first**;禁止截断缓存覆盖权威;修 sync 合并语义
3. **队列超限丢最旧或 WAL**;注释与行为对齐;信号路径可等待 flush
4. **修 `ensureLock` finally**;修 doctor `models-store.json`
5. **user 轮持久化或文档降级主张**

#### 短期(安装/安全闭环)

6. 补 Release:`celld-linux-x64`(+ darwin-x64);重发与 main 一致的 `install.sh`
7. install 增加 **SHA256 校验**;worker tar 防路径穿越;固定 esbuild 版本
8. worker 变更 API 加本机共享密钥;移除或门禁 `webhook-test`
9. 停止 `readFileSync` credentials;endpoint 白名单;`projectTrusted` 默认 false
10. `engines.node: ">=22.19.0"`

#### 中期(产品化)

11. 拆分 `celagent-tui.mjs`;统一 awsEnv;拆分 bosWarned
12. history_search 默认限当前会话;settings 合并写;own.json 仅清过期本节点
13. ledger record-before-side-effect;alarm 统一调度
14. CI release job(匿名路径 bun build + 上传)

### 12.8 覆盖矩阵(本评估已查 / 未查)

| 域 | 状态 |
|----|------|
| 文档↔代码一致性 | ✅ 三轮 |
| 写路径 CAS/队列 | ✅ |
| 读路径/恢复/sync | ✅ |
| CLI/doctor/config | ✅ |
| install/setup/node/cluster | ✅ |
| worker 任务/ledger/alarm | ✅ |
| 安全(凭证/tmp/供应链/本机 API) | ✅ |
| Release 资产 HTTP | ✅ |
| 单测/CI 行为 | ✅(无 BOS/celld 真联调) |
| 真实 BOS 压测 / 多机故障注入 | ❌ 本环境无凭证与 celld |
| Bun 二进制交叉编译复现 | ❌ 未跑 |
| Windows 路径 | ❌ 资产缺失 |
| pi 上游 API 兼容性矩阵 | ❌ 仅锁 v0.84.x |

### 12.9 终局判断

三轮排查后综合分校准为 **6.8/10**。

项目最大资产仍是:**把 durable session 当一等公民的架构选择 + 写路径工程纪律**。  
最大风险是:**对外 RPO 叙事超前于实现**,叠加 **CI/Release 未闭环** 与 **本机安全默认偏松**。

若只做一个里程碑:「合入 PR#1 → 修 C1/C2/C3/ensureLock/doctor → CI 绿 → 补 celld-linux → 改 README 保证口径」,完成后综合分有望回到 **8-**。

---

## 13. 第四轮排查(UX / 打包 / 演示页 / 可执行证据)

> 范围:CLI UX、`/fork` 持久化、npm 打包、演示页口径、bos-compat 检查数、
> Critical 逻辑的**可执行最小复现**(`tests/review-logic-proofs.test.mjs`)。

### 13.1 可执行证据(本轮已跑通)

```text
$ node --test tests/review-logic-proofs.test.mjs
# 5 pass — 固化:
#   R3-C3 队列丢最新
#   R3-M1 length≠max(turn)
#   同 turn replace 抹 content
#   R3-H1 ensureLock 早退后仍持锁
#   /fork 不在 persistId 切换集合内
```

结构证据:`ensureCelld` 中 `ensureLock = null` **仅 1 处**,落在深层 `try/finally`;
健康节点 `if (r.ok) return` 与无配置 early-return **绕过 finally** → 锁泄漏成立。

### 13.2 新发现 — `/fork` 串写(High)

| ID | 严重度 | 问题 | 证据 |
|----|--------|------|------|
| R4-1 | **High** | `/fork` 不切换 `persistId` | 代码仅处理 `reason === "new" \|\| "resume"`;注释提到 fork「新上下文」却未换 BOS key |
| R4-2 | **High** | 文档三方口径冲突 | README/architecture §1.4:**BOS-first**; demo HTML + 代码:**worker-first**; demo 还声称截断「安全」 |
| R4-3 | Medium | `rm` 无非 TTY / `--yes` | `readline.question` 卡住 CI/管道 |
| R4-4 | Medium | `list` denylist 误伤真实会话名 | `default`/`debug`/`bos-*`/`*-test` 等被隐藏 |
| R4-5 | Medium | `config set` 无校验 | 可把 `persistence` 写成标量毁掉嵌套 |
| R4-6 | Medium | npm pack **仅 13 文件**,不含 `docs/` | README 指向的架构/评估文档在 npm 安装物中不存在 |
| R4-7 | Medium | `engines: >=22` vs 依赖 `>=22.19.0` | 22.14 已 EBADENGINE |
| R4-8 | Low–Med | `exports` 仅 `./bos` | `bos-tools` 无法被库消费者 import |
| R4-9 | Low–Med | `worker/wrangler.jsonc` Cloudflare 形态 | 实际 `celld deploy`;易误导贡献者跑 wrangler |
| R4-10 | Low | bos-compat 写「17 项」,脚本自称「6 项链路」 | 实际 `check()` 约 10 次 + wake 无条件 PASS |
| R4-11 | Low | doctor `[2/5]` env 凭证也显示「[bos] profile」 | 文案误导 |
| R4-12 | Low | doctor「缺缺失」错别字 | `缺缺失` |

### 13.3 演示页的双重角色

演示页对实现更诚实(写明 worker 快路径 + 200 截断),但同时:

1. 把 worker-first 包装成「历史完整 RPO=0」→ **产品营销 overlay**
2. 声称「截断安全,因 BOS 有完整」→ **忽略恢复根本不读 BOS 的热路径**
3. 与 README「恢复优先级:BOS 是权威源」**直接打架**

**建议**:演示页恢复段改为「当前实现 worker-first(截断风险);目标语义 BOS-first」,或改代码后改回 demo。

### 13.4 打包与分发面

| 渠道 | 内容 | 缺口 |
|------|------|------|
| GitHub Release | 二进制 + install + worker + 部分 celld | 缺 linux/darwin-x64 celld、windows;install≠main |
| npm pack | 13 文件 runtime | 无 docs/HANDOFF/评估;engines 过松 |
| 源码 clone | 完整 | 依赖 celld 外置 + BOS 凭证 |

### 13.5 四轮后的问题全景(按主题去重)

```
正确性 ── C1 worker覆盖权威 / C2 sync抹新轮 / C3 丢最新
           /fork串写 / 仅assistant / seq=length / bosGet TOCTOU
可用性 ── ensureLock泄漏 / doctor假阴性 / rm非TTY / list误过滤
安全   ── 无checksum安装 / worker无鉴权 / /tmp(PR1已修) / 凭证读堆
           endpoint无白名单 / projectTrusted / history跨会话
工程   ── CI扫node_modules / 测试ENOENT / engines / Release漂移
文档   ── BOS-first vs worker-first 三方冲突 / 17≠实际检查数
```

### 13.6 更新优先队列(插入 R4)

在 §12.7「立即」清单中插入:

- **修 `/fork`**:与 `/new` 同等生成独立 `persistId` 并提示用户
- **统一恢复口径**:改代码 BOS-first *或* 改 README/demo/architecture 三处一致,并撤销「截断安全」断言
- **把 `tests/review-logic-proofs.test.mjs` 纳入 CI**(零外部依赖,应强制通过)

### 13.7 评分(第四轮后)

| 维度 | R3 | R4 | 变化 |
|------|----|----|------|
| 核心持久化正确性 | 6.5 | **6.0** | `/fork` 串写 + 可执行证据固化 C3/replace |
| 文档一致性 | 7.5 | **7.0** | 确认 README↔demo↔code 三方冲突未解 |
| 测试与 CI | 4.0 | **4.5** | 新增零依赖 proof 测试(待接入 CI) |
| **综合** | 6.8 | **6.7** | 问题面更完整,可信度叙事更收紧 |

### 13.8 本轮明确不重复深挖的项

- 真实 BOS/多机故障注入(无凭证)
- Bun 交叉编译与 Windows
- pi 上游全版本兼容矩阵

上述仍是「发布前人工验收」清单,不阻塞本评估文档结论。
