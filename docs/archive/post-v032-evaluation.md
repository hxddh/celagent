# v0.3.2 之后 — 架构评估(下一刀排期)

> 评估对象:v0.3.2 已发布之后,HANDOFF「后续候选」以及源码里还没单独评估过的缺口。
> 配套:`docs/s3-compat-evaluation.md`(存储多后端,本 PR 同期)、`docs/celld-v02-evaluation.md`(v0.2 利用,P0 已落地)。
> **本 PR 只评估 + 修正过时交接信息,不改运行时代码。**

## 0. 结论先行

**下一刀实现仍然是存储 P0**(fail-closed + 配置单一来源),不要插队做 UI 或「多 provider 认证管理」。

HANDOFF 列出的四个未排期方向,源码对照后的结论:

| 候选 | 结论 |
|------|------|
| 对象存储多后端 P0 | **做**,已有完整评估 |
| 多 provider 认证管理 | **别做产品**。pi 引擎已管 models/auth;celagent 只同步 `provider`/`model` |
| 快照浏览 UI | **最多加 CLI list**,不要新 TUI 面 |
| 会话 diff/合并 | **做错抽象**。权威路径已是 CAS + 同轮替换,不缺三路合并 |
| Bucket 生命周期 | **运维手册,不是功能**。不要自动删权威会话 |

更值得写进债、但不必立刻开功能 PR 的:worker token 空值 fail-open、未调用的 SigV4、HANDOFF 曾把 Latest 写成 v0.3.1、CI 证明不了 RPO=0、记忆检索无索引。

## 1. 评估地图(避免重复劳动)

| 文档 | 覆盖 | 落地 |
|------|------|------|
| PR #2 `project-evaluation`(未进 main) | 正确性/安全/发版 | v0.3.1,对照见 `evaluation-followup.md` |
| `celld-v02-evaluation.md` | 双监听、token vars、drain、diagnose、调参 | **v0.3.2** |
| `s3-compat-evaluation.md` | 合格存储门禁、静默退回 BOS | **未改代码** |
| 本文 | v0.3.2 后排期、候选方向真伪、测试与卖点的缝 | 交接文档纠偏 |

v0.3.2 利用评审里明确推迟的:OTEL、桶 prefix、Loader/WS/Wasm/RPC、GCS。prefix 的正确用法见 §3.5(不必搬会话)。

## 2. 发布交接纠偏

评估时 GitHub Latest 已是 **[v0.3.2](https://github.com/hxddh/celagent/releases/tag/v0.3.2)**(tag 对象 `e14729e` → 提交 `e5ae737`,PR #7 合并)。HANDOFF §3 仍写「Latest v0.3.1」——这是交接事故,不是产品债。本 PR 已改 HANDOFF。

资产形态与 v0.3.1 相同(celagent 五平台 + celld linux/darwin-arm64 + SHA256SUMS);**差的是 runtime 能在 celld 0.2.0 上启动**。不要用 v0.3.1 二进制对 0.2.0 节点。

## 3. HANDOFF 候选方向(逐项)

### 3.1 多 provider 认证管理 — 别做

现状:

- LLM 走 pi:`createAgentSessionServices` + `agentDir=~/.config/celagent/pi-runtime`(settings / auth.json / models-store.json)。
- 文档主路径:`DEEPSEEK_API_KEY`;无 key 时 pi 侧降级(README 写 mock)。
- celagent `config set provider/model` 只同步到 pi-runtime(Bug 87)。**真正鉴权文件是 pi 的 `auth.json`**,celagent 不解析。

「认证管理」若做成 celagent 功能,等于 fork 一份 pi 已有的 provider 配置,还要处理 key 落盘——和「仓库零凭证、不 fork pi」红线冲突。

可做的小项(都不叫「认证管理」):

- `doctor` 增加一维:pi `auth.json` 是否存在 / `DEEPSEEK_API_KEY` 是否在环境(只报有无,不打印值)。
- README 写清:换 Claude/OpenAI 用 pi 自己的登录/配置,不是 `celagent config set apiKey`。

### 3.2 快照浏览 UI — 不要新交互面

`session_snapshot` 把当前 turns 写到 `snapshots/<name>-<ts>.json`,**没有 list/get/rm CLI**,agent 之后也搜不到(history_search 只扫 `sessions/`)。

这是工具半成品,不是缺一个 GUI。

| 做法 | 建议 |
|------|------|
| `celagent snapshot list/export` | P2,几十行,复用 bosGet |
| 让 history_search 可选扫 snapshots/ | 可选,注意默认仍当前会话 |
| 独立浏览 TUI | **不做**。和「CLI agent」定位无关,celld static assets 也别接演示页 |

### 3.3 会话 diff/合并 — 做错抽象

并发模型已经是:

- 进程内单写者队列;
- 跨进程 `If-Match` + 412 重读重试;
- 同 `turn` 存在则替换(幂等)。

缺的不是 diff3。若两个 celagent 写同一 session id,CAS 让一方重试,合并语义是「后写的那轮替换同序号」,不是 git merge。

做会话 diff 的唯一合理场景:用户想对比两个 **不同** session id(或 snapshot vs 权威)。那是 `celagent export` + 外部 diff,不必进产品。

### 3.4 Bucket 生命周期 — 运维不是功能

权威会话 **故意不截断**。对象会随轮次变大;steer 只注入最近 50 轮,完整 JSON 仍在桶里。

自动 lifecycle 删 `sessions/` = 破坏 RPO=0。可写进部署文档的运维建议:

- 对 `snapshots/`、`workspace/` 设过期(若用);
- `sessions/` 只手动 `celagent rm`;
- doctor 可对单个会话对象 size 打警告(P2),不自动删。

celld 的 L1 压缩已经在降 `cells/*/ltx` 的 LIST 成本,与会话 JSON 无关。

### 3.5 celld 桶 prefix — 不要绑会话迁移

Codex 对存储评估的纠正成立,已写回 `s3-compat-evaluation.md`:

celld `--bucket s3://bucket/celagent` 只把 **执行层** 对象放到前缀下。会话客户端继续用桶根 `sessions/*`。两者从不互相枚举。新 fleet 隔离不必搬历史会话。

## 4. 源码里更值得记的债(不在候选列表)

### 4.1 worker 手写 SigV4 是死代码

`worker/src/index.js` 的 `bosPut`/`bosGet` 只定义不调用。`obj-put` 与任务完成写 `workspace/` 都走 `bosPutProxy` → `CELAGENT_WEBHOOK_BASE`(默认环回 19090)。

扩 S3 后端时适配这条路径是浪费。落地存储 P0 时或单独清扫 PR:**删除这两个函数**(~70 行)即可。

### 4.2 token 空值仍 fail-open

```168:170:worker/src/index.js
function checkToken(req, env) {
  const expected = env && env.CELAGENT_WORKER_TOKEN;
  if (!expected) return true;
```

`wrangler.jsonc` 的 `vars.CELAGENT_WORKER_TOKEN` 是 `""`。空字符串为假 → 未注入时鉴权关闭。v0.3.2 已在 spawn 设 `CELLD_VAR_CELAGENT_WORKER_TOKEN`;setup/node_mgr 也会生成 token。

**本机环回 Worker 口**下 fail-open 可接受(未配 token 时 TUI 仍能 checkpoint)。不要在无威胁模型变化时改成 fail-closed——会让「没跑过 setup 的开发者」全部 401。

若以后 `--listen` 绑到非环回,必须 fail-closed。当前 spawn 数据面是 `127.0.0.1:18090/18091`,与 fail-open 一致。

### 4.3 记忆检索没有索引

`history_search` 跨会话时 `list-objects-v2 --max-items 40` + 全量 GET + 子串匹配。会话变多后会慢、会漏。这是工具实现上限,不是架构裂口。P2 以前不要上向量库/旁路索引(又一套真相)。

### 4.4 CI 证明不了 RPO=0

CI:`node --check`、help/version、`npm test`(无节点则 skip Celld/BOS)、secret 扫描、`bash -n`。proof 测试锚定源码字符串,包括曾经把「静默退回 BOS」当成正确行为。

产品承诺的验证面仍是 **有 BOS 凭证的机器** + `celld-bos-test.sh`。这不是 CI 能假装补上的。存储 P0 的 fail-closed **可以**在无凭证 CI 里测(非法 endpoint 必须拒绝)。那是 P0 落地的验收项,不是新测试框架。

### 4.5 多机文档的故障步骤

`docs/distributed-deployment.md` 仍用 `pkill` 演示崩溃。v0.3.2 的正确停机是 drain。应在改文档时把「模拟崩溃」和「滚动重启」拆开(P1 文档,不挡存储 P0)。

`docs/bos-compat.md` 的 RestoreFailed/~10s 窗口应按 v0.2 output gate 降级口径,避免新 agent 按 v0.1 运维。

## 5. 推荐排期

```
现在(评估 PR)     文档:存储评估 + 本文 + v0.3.3 范围合同
下一刀实现        **v0.3.3** — `docs/v033-scope.md`
再下一刀          v0.3.4:CAS doctor + 删 SigV4 死代码;可选 snapshot list CLI
更后 / 有人要     R2 实测、OTEL opt-in、celld 桶 prefix(不搬会话)、
                  doctor 查 pi auth 有无、attestation verify
不要排期          自研 provider 认证、快照 TUI、会话 merge、自动删 sessions/、
                  MinIO 当 HA、会话迁进 celld、Worker Loader/WS
```

版本号建议仍跟存储评估:P0 单独可发 **v0.3.3 或 v0.4.0**。本文不占版本号。

## 6. 明确不做

- 为「看起来更像平台」加认证中心、Web 控制台、会话合并器。
- 用 CI 假绿替代 BOS 机上的 CAS 实测。
- 把 wrangler 空 token 改成强制 401(在数据面仍环回的前提下)。
- 评估阶段改 `src/` `bin/` `scripts/` 行为。

## 7. 源码索引

| 路径 | 与本文关系 |
|------|------------|
| `bin/celagent-tui.mjs` | pi-runtime 同步、steer 50 轮、ensureWorkerToken |
| `src/bos-tools.js` | snapshot 只写不列;search 最多 40 会话 |
| `worker/src/index.js` | 死代码 SigV4;checkToken 空值放行;产物 webhook |
| `worker/wrangler.jsonc` | `vars.CELAGENT_WORKER_TOKEN=""` |
| `HANDOFF.md` §3–4 | 发布状态(本 PR 已对齐 v0.3.2);候选方向以本文为准 |
| `docs/distributed-deployment.md` | pkill 演示待拆 |
| `tests/core.test.mjs` | 无节点/无桶 skip,CI 不跑 CAS |
