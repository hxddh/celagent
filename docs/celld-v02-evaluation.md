# celld v0.2.0 × celagent 深度评估

> 评估对象:上游 [denoland/celld v0.2.0](https://github.com/denoland/celld/releases/tag/v0.2.0)(2026-08-12) 对 celagent v0.3.1 的冲击。
> 依据:官方 Release notes、[celld.dev/docs](https://celld.dev/docs)、本机 `celld 0.2.0 -h` 实测、celagent 源码对照。
> **评估时**(v0.3.1 树)所有 spawn 仍是 v0.1 单监听,官方 0.2.0 二进制会拒绝启动。
> **同 PR 已落地 P0**:数据面 18090/18091,控制面 port+2,advertise=内部地址。P1 停机/drain/diagnose 未做。

## 0. 结论先行

**必须迭代 celagent。** v0.3.1 Release 已经随包分发 **celld v0.2.0**,但所有拉起节点的命令仍是 v0.1 单监听:`--listen … --advertise <同一地址>`。

v0.2 实测:

```
Error: --advertise or CELLD_ADVERTISE requires an explicit --internal-listen
       or CELLD_INTERNAL_ADDR; the default internal listener uses a random loopback port
```

因此 `ensureCelld` / `node_mgr` / `setup.sh` / `install.sh` / `cluster_mgr` **在 v0.2.0 上会启动即退出**。这不是优化项,是兼容性断裂。

其余:BOS 仍应是会话权威源;celld 作为执行层在 v0.2 明显变强(租约、压缩、接管钉点、优雅退出)。平台包仍无 Intel Mac / Windows。

## 1. v0.2.0 改了什么(三根基)

官方原话:how cells share memory, how their state replicates, how a fleet stays available when its object store fails or slows.

### 1.1 隔离与内存

| | v0.1.0 | v0.2.0 |
|--|--------|--------|
| 驻留 cell | 一 cell 一 OS 线程 + 一 isolate | 共享 isolate 池,按 turn 调度 |
| 驻留成本 | ~3.4 MB/cell | ~471 KB/cell(测到 2500 驻留 / ~1.2 GB) |
| await I/O | 占住 isolate,邻居可能被拖 | 释放 isolate,共驻 cell 互不堵 |
| 泄漏 | 长寿命节点可能漏 isolate | 回收空 heap、复用 isolate slot |
| 过载 | CPU shedding + RSS | **去掉 CPU shedding**,只留 RSS(默认开) |
| 满槽无状态请求 | 立即拒绝 | 等待,上限 `CELLD_ADMISSION_WAIT_MS` |

对 celagent:双节点不再是为了「内存装不下」,而是为了本机 HA。单机一个节点 + RSS shedding 也能撑很多 session cell。仍建议保留双节点做接管演练。

### 1.2 双监听(数据面 / 控制面)

- **`--listen`(数据面)**:只服务 Worker 路由 + `/__celld/health`。drain 时 health 变 503。
- **`--internal-listen`(控制面)**:operator API(`/state`、`POST /shutdown`、`/evict`…) + 节点互访。默认 `127.0.0.1:0`(随机环回端口)。**无鉴权**,禁止暴露到公网。
- **`--advertise`**:peers 用来连 **内部监听**,不是 Worker 端口。显式 advertise **必须**显式 internal-listen。
- 非环回 `--listen` 必须带 `--internal-listen`(用来识别过时的单监听配置)。

celagent 以前把 `--advertise` 设成与 `--listen` 相同。v0.2 下:进程拒绝启动;即便绕过,peer 流量也会打到已经没有 peer API 的数据面。

### 1.3 复制与压缩

- 默认把大量 L0 小对象压成 **L1 block**(LTX v0.5.2)。**不删源对象**。
- 接管从「读数千对象」变成「读几十个」。官方一例:4096 次 durable 写,从 4116 对象 / 19 MB → 42 对象 / 0.6 MB,快 3.8×。
- 接管 **钉住** 恢复点:fenced 旧 owner 事后上传的数据不会进新历史 → cell 历史不能分叉。
- 压缩在请求路径外,有 duty cycle;重启时积压不会挡住 durable 写。
- `CELLD_LTX_COMPACTION` 默认 `1`。混部旧 reader 必须全员关压缩,否则第一次 L1 发布后旧节点无法接管。

对 `docs/bos-compat.md` §二.6(强杀 → RestoreFailed,「checkpoint 不等 LTX」):v0.2 默认 **`CELLD_OUTPUT_GATE=1`**(ACK 前证明 durable)。强杀窗口应显著缩小,但仍应用 SIGTERM drain,而不是 `pkill` 后 2 秒再拉起。

### 1.4 存储故障下的可用性

- 租约续约尝试被「上次续约剩余时间」卡住,一次卡住的条件写不能吃完整权威窗口。
- 官方:静默丢掉存储连接 20s,v0.1 节点被 fence,v0.2 继续服务。
- 租约定时器在独立线程,restore/压缩/诊断不能拖死它。

BOS 偶发超时不再等于「整个执行层挂掉」。会话权威仍应走 celagent 的 BOS 直写;celld 更适合当缓存/任务/集群。

### 1.5 关机与重启

- SIGTERM/SIGINT → 优雅 drain:health 503,把 resident cell **直接交给 peer**,不必等 lease TTL。
- `POST /shutdown?handoff=preserve`(内部监听):同机重启,保留 ownership,不读 owner/replica。
- `CELLD_SHUTDOWN_DRAIN_MS` 默认 25000。celagent `pkill; sleep 2; start` 会在 drain 未完成时再拉起两个进程抢租约,并配合 `own.json` 全量删除 — 与 v0.2 的钉点接管 **对着干**。

### 1.6 配置破坏性变更

| 项 | v0.2 |
|----|------|
| 布尔 env | 只接受 `0`/`1`,非法值 **拒绝启动** |
| 删除 | `CELLD_WORKERS`、`CELLD_MAX_COHOSTED`、`CELLD_MAX_CPU_PERCENT`、`CELLD_RESIDENT_LOW_WATER`、`CELLD_VALIDATE`、`/js` |
| 改名 | `CELLD_HIBERNATIONS` → `CELLD_EVICTIONS` |
| `CELLD_IDLE_EVICT_S` | **不再默认 300;不设则关闭 idle eviction** |
| `CELLD_UNSAFE_PUBLIC_ADVERTISE` | 取值 `1`,不是 `on` |
| 升级 | **禁止滚动**:停光所有 v0.1,再启 v0.2。混部会 peer 对不上、L1 旧节点读不了 |

celagent 源码未使用已删变量。`node_mgr` 已显式 `CELLD_IDLE_EVICT_S=30`;`ensureCelld`/`setup.sh`/`install.sh`/`cluster_mgr` **没设**,v0.2 下 cell 会常驻(直到 RSS shedding)。

### 1.7 其它能力(迭代可选)

- 桶可带 **key prefix**(`s3://bucket/prefix`),多 fleet 共享一桶。
- `gs://` + ADC。
- `celld diagnose`(签名探测 peer、restoring=0、错误 advertise)。
- `CELLD_OTEL=1`:telemetry/ 下 Parquet,或 OTLP。
- Wasm worker、更完整 Web Crypto / `node:crypto`(celagent worker 已用 `crypto.subtle` HMAC,收益有限)。
- `setWebSocketAutoResponse`(celagent 无 WS 入口,暂无关)。

### 1.8 上游仍未提供

官方 limitations + v0.2.0 资产仍是三件套:

- `celld-x86_64-unknown-linux-gnu.gz`
- `celld-aarch64-unknown-linux-gnu.gz`
- `celld-aarch64-apple-darwin.gz`

**无** Intel Mac、**无** Windows。与 v0.3.1 已知边界一致。

凭证文档有张力:README 写 standard AWS credential chain;`limitations` 写不读 `~/.aws` profile。celagent 全靠 `AWS_PROFILE=bos` 且 unset 显式 AK/SK。需在有 BOS 的机器上用 v0.2.0 二进制确认 profile 仍可用;若不能,只能从 `aws configure get` 注入环境(卫生上更差)。

## 2. 冲击矩阵(对照 celagent 源码)

| 优先级 | 点 | 现状 | v0.2 后果 | 迭代 |
|--------|----|------|-----------|------|
| **P0** | 启动参数 | `--listen`+`--advertise` 同端口,无 `--internal-listen` | **拒绝启动**(已用 0.2.0 二进制核实) | 数据面 18090/18091;控制面 **port+2**(18092/18093);advertise=内部地址 |
| **P0** | 多机 `cluster_mgr add-node` | listen 环回,advertise 填 LAN:worker 口 | 显式 advertise 无 internal → 拒启;即便启了 peer 打错面 | listen 仍环回;internal-listen/advertise 绑 LAN:port+2;公网 IP 才加 `--unsafe-public-advertise` |
| **P1** | 停/重启 | `pkill; sleep 2` + 可能 `own.json` 全删 | drain 默认 25s;抢租约;对抗 takeover pin | SIGTERM 等到 health 非 200 或进程退出;优雅重启走 `handoff=preserve`;own.json 只留给崩溃残留 |
| **P1** | idle eviction | 仅 node_mgr 设 30s | 其它路径 cell 常驻 | 统一 `CELLD_IDLE_EVICT_S`(本机 30 合理) |
| **P1** | 运维可观测 | `curl health` + 列 `nodes/` | 不够 | `celld diagnose`;doctor 打内部 `/state`(仅环回) |
| **P1** | 文档/测试 | bos-compat、distributed-deployment 按 v0.1 写 | 过时 | 压缩、output gate、禁止混部、故障演练改 SIGTERM |
| **P2** | 桶 prefix | `s3://bucket` 根路径 | 可 `s3://bucket/celagent` 隔离 | 可选,迁移要搬对象 |
| **P2** | OTEL | 无 | `CELLD_OTEL=1` 写桶 | 默认关 |
| **P2** | 单节点模式 | 固定双节点 | 内存不再是理由 | 可做成「1 热 + 1 冷」或可关第二节点 |
| **P2** | GCS | 只 BOS | `gs://` 已一等 | 不挡 BOS 主路径 |
| **—** | 会话权威 | BOS-first + CAS | 仍然正确 | **不要**改回 worker-first |
| **—** | 平台包 | 无 darwin-x64/Windows | 仍无 | 继续回退 celld.dev / 跳过 |

命中文件:`bin/celagent-tui.mjs` `ensureCelld`、`scripts/node_mgr.sh`、`scripts/cluster_mgr.sh`、`setup.sh`、`install.sh`、`scripts/celld-bos-test.sh`。

## 3. 架构还成不成立

```
TUI  →  BOS sessions/*.json     权威,RPO=0,不依赖节点
     →  celld worker SQLite     缓存 / 任务 / 休眠 / 跨节点委托
```

**仍然成立,而且更该这样分层。**

v0.2 让 cell 历史更难分叉、租约更抗存储抖动、接管更快。这增强的是 **执行层**,不是「把会话 JSON 只放在 cell 里就够了」:

- 会话是给模型用的文本轮次,要跨机器、跨 celagent 版本、在节点全挂时仍可读。BOS 对象 + CAS 已经验证。
- cell SQLite 适合任务状态机、ledger、alarm。output gate 让这一层的 ACK 更接近 RPO=0,但仍受 worker msg 上限与部署绑定约束。
- 双写(BOS 权威 + worker 缓存)的成本仍值得。

应改的叙事:

- 「强杀后必须扫 own.json」从默认运维变成 **崩溃恢复**;正常停机用 SIGTERM/drain。
- 「lease 10s 才接管」在有 peer 时被直接 handoff 缩短。
- 「idle 300s 休眠」不再是默认;要休眠必须显式 `CELLD_IDLE_EVICT_S`。

## 4. 建议迭代顺序

1. **P0(本 PR 落地)**:所有 spawn 补 `--internal-listen`,advertise 改为内部口(约定 public+2)。TUI 仍只打 `http://127.0.0.1:18090|18091` 的 Worker/health。
2. **P1(下一刀,需 BOS 机)**:停机等待 drain;`handoff=preserve`;收紧 own.json 清理;`celld diagnose`;补测 failover;确认 `AWS_PROFILE=bos`。
3. **P1 文档**:bos-compat / distributed-deployment 按 v0.2 重写故障步骤。
4. **发版**:P0 进树后应切 **v0.3.2**(v0.3.1 随包的 celld 已经是 0.2.0,但启动参数是错的)。**禁止** v0.1/v0.2 节点混部。
5. **P2**:OTEL、桶 prefix、可关第二节点、GCS。

不要做的:

- 不要把会话权威迁回 celld。
- 不要滚动升级混部。
- 不要把内部口暴露到公网,也不要把 `--advertise` 指回 Worker 口。
- 不要发明 Intel Mac/Windows 的 celld 包。

## 5. 本环境验过 / 没验过

**已验**

- `celld 0.2.0` linux-x64 二进制 `--version`
- `--advertise` 无 `--internal-listen` → 报错拒绝
- v0.2.0 GitHub 资产仍无 darwin-x64 / Windows
- celagent 全部 spawn 点都是旧双参数

**未验(要有 BOS + 真机)**

- 加上 internal-listen 后双节点能否互相接管
- SIGTERM drain vs own.json 清理
- 压缩后 BOS `cells/*/ltx` 形态
- `AWS_PROFILE=bos` 在 v0.2 是否足够
- 多机 advertise 私网 IP
- 20s 存储中断是否仍能服务(官方数字,本仓未复现)
