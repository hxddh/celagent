# v0.4.3 范围(本刀实现)

> 版本号 **`0.4.3`**。依赖组件全面跟随上游最新,并显式采纳新能力。
> **本文件是实现合同**。默认仍是 BOS。**不宣称**已支持 R2/S3/Azure。
> 不移动 v0.3.x / v0.4.0–v0.4.2。非 BOS 真桶实测仍在 `docs/v038-scope.md` 排队(需凭证)。

## 用户能感知什么

- 随包 celld 从 v0.2.0 升到 **v0.3.0**(上游最新),带来其 v0.3 的修复与新能力。
- Pi 引擎升到 **0.84.3**(上游最新)。
- 二进制体积略降:移除了一个从未被 import 的重复依赖。

## 做

### 依赖升级(全部跟随上游最新)

| 组件 | 之前 | 现在 | 依据 |
|---|---|---|---|
| `@earendil-works/pi-{coding-agent,agent-core,ai}` | 0.84.1 | **0.84.3** | 上游最新;`npm test` 全绿 |
| `celld`(随包分发) | v0.2.0 | **v0.3.0** | 上游最新;参数面兼容性已实测 |
| `esbuild` | 0.28.2 | 0.28.2 | 已是上游最新 |
| `chalk`(直接依赖) | ^5.6.2 | **移除** | 见下 |

**chalk 为何是移除而不是升到 6.0.0**:本仓库源码**从不 import chalk**(只在一句注释里提到);
而 pi 把 chalk **精确钉死在 5.6.2**。把我们的声明抬到 ^6 只会让包里出现两份 chalk,
顶层那份没有任何代码使用,徒增二进制体积。移除后**我们不再为它声明任何版本**,
由 pi 自行解析——这比钉一个我们用不到的版本更彻底地消除了"版本停滞"。
实测:移除后 `bun build --compile` 正常(1817 模块),二进制 `version`/`help` 均正常。

### 采纳 celld v0.3 新能力(显式声明,不吃默认值)

6 个 spawn 点(`bin/celagent-tui.mjs`、`setup.sh`、`install.sh`、
`scripts/{node_mgr,cluster_mgr,celld-bos-test}.sh`)统一加:

- **`CELLD_DURABILITY=fleet`** — celld v0.3 新增的耐久性档位(`fleet` = follower-fsync
  quorum 即 ack、后台向桶分层;`bucket` = 等存储落盘)。取 `fleet` 的依据是架构 ADR 1/3:
  **会话权威写直连对象存储、不经 celld**,celld 只持有"丢了可重建"的缓存,不该为它付等存储的延迟。
- **`CELLD_STORAGE_PROBE=1`** — 让 celld 启动即自验条件写。本产品全部承诺建立在条件写之上,
  与 `cas-probe` 门禁同向。

**显式写出来的理由**:这两项都有上游默认值,不写也能跑。写出来是为了**上游改默认值时
不会悄悄改变我们的耐久性与自验姿态** —— 本项目被上游版本变更咬过(v0.3.1 用 v0.1 spawn 参数)。

### 明确不采纳(逐条给理由)

- **`--trust-forwarded-headers` / `CELLD_TRUST_FORWARDED_HEADERS`** — 让 `X-Forwarded-Host/Proto`
  决定 request.url 的 scheme 与 host。本产品节点仅环回监听,启用只会削弱来源可信度。
- **`az://`(Azure Blob)** — 无实测不宣称,与 BOS 之外后端的既定纪律一致(见 `docs/v038-scope.md`)。
- **`celld d1` 子命令** — 本产品不用 D1。
- **`--read-only` / `CELLD_STORAGE_PROBE=0`** — 与上一条相反,我们要的正是探针开着。

## 兼容性怎么证明的(不是读 changelog)

1. `--help` 逐行 diff v0.2.0 vs v0.3.0:**纯增量、零移除**。
2. 对二进制 `strings` 核验 celagent 实际传的 5 个 env 全部存在
   (含 `CELLD_ADMISSION_WAIT_MS` —— 它真实存在,只是未写进 `--help`)。
3. **用 celagent 的完整 flag+env 组合实跑 v0.3.0**:全部参数被接受,
   一路解析到 bucket 校验才因沙箱网络停下,无任何 unknown/invalid 参数错误。

## 明确不进 v0.4.3

- 真 R2/S3/Azure 联调(→ `docs/v038-scope.md`,需凭证机器)
- 分段存储 / 多端同写(→ 大版本讨论)
- 把 `CELLD_UPSTREAM_TAG` 改成动态 "latest":发布流水线产出签名二进制 + `SHA256SUMS`,
  构建时拉"最新"会让同一 tag 的产物随上游漂移,破坏可复现与供应链完整性。
  正确做法是**钉当前最新 + 定期跟随**,本刀即是一次跟随。

## 验收

- [x] pi 三件套 = 上游最新 0.84.3,`npm test` 全绿
- [x] celld 随包 tag = 上游最新 v0.3.0,参数面兼容性三重实测
- [x] chalk 直接依赖移除,`bun build --compile` 与二进制实跑通过
- [x] 6 个 spawn 点显式声明 `CELLD_DURABILITY` / `CELLD_STORAGE_PROBE`
- [x] 源码锚定:无过时钉版、新能力已声明、不得启用转发头信任
- [x] 版本号 0.4.3(package.json / TUI / install.sh)

## 发版

实现合进 `main` 后打 tag **`v0.4.3`**(不移动旧 tag)。随包 celld 为 **v0.3.0**。
