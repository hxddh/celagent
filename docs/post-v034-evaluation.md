# v0.3.4 之后 — 下一版评估

> 评估对象:Latest **[v0.3.4](https://github.com/hxddh/celagent/releases/tag/v0.3.4)**(`eec47c4`) 之后,存储路线还剩什么、v0.3.5 该不该仍是「非 BOS 实测」。
> 配套:`docs/s3-compat-evaluation.md`(合格门禁)、`docs/v034-scope.md`(已发布合同)、`docs/post-v032-evaluation.md`(候选方向真伪,结论仍成立)。
> **本文只评估,不改运行时代码。**

## 0. 结论先行

**v0.3.5 仍应是:至少一种非 BOS 合格后端的真桶实测(建议 Cloudflare R2)。** 不是再做一层 doctor,也不是用 CI 内存 store 宣称「已支持」。

v0.3.3 / v0.3.4 已经把「配错写到百度」和「忽略 If-Match 仍当权威源」这两条产品缺陷关掉。存储评估 P1 还剩 **真桶走通 + 文档改叙事**。没有真桶,版本号不该前进。

本云环境 **没有** `~/.aws/credentials`、没有 `AWS_*` / R2 token。实现 PR 在拿到凭证前无法诚实完成验收。不要用 MinIO / LocalStack / 内存 mock 顶替。

| 问 | 答 |
|----|----|
| 下一刀还是 R2 实测吗 | **是**。HANDOFF 已锁,P1 只剩这一项 |
| 没凭证能不能先发「文档版 v0.3.5」 | **不能**。没有实测就改 README「已支持」是交接事故 |
| 顺手做 snapshot list / 认证 UI / GCS 吗 | **不**。与卖点无关,评估里已否决过 |
| 内存 CAS 探针算不算非 BOS 实测 | **不算**。它只证明探针逻辑,不证明 R2/S3 |

实现合同见 `docs/v035-scope.md`。

## 1. 已落地 vs 还没做

存储评估原切分(见 `s3-compat-evaluation.md` §7)和实际发版有错位,以发版为准:

| 项 | 计划 | 实际 |
|----|------|------|
| fail-closed + settings 单一来源 + 白名单 | v0.3.3 | ✅ v0.3.3 |
| CAS doctor / persist 拒绝 / 删 SigV4 | 原写入 v0.3.4,并带「R2 实测」 | ✅ 门禁在 v0.3.4;**实测拆到 v0.3.5** |
| 真 R2 或 S3:配置 → CAS → 写会话 → 另一进程恢复 | P1 | ❌ 未做 |
| 第一批后端 settings 样例(已测/未测分开) | P1 文档 | ❌ README 仍只保证「URL 能配进去」 |
| GCS / prefix / rename `bos.js` | P2 | 仍更后 |

v0.3.4 验收过的是:内存 store 遵守 CAS → 通过;忽略 If-Match → `cas-ignored`。这是门禁正确性,不是后端矩阵。

## 2. 为什么下一刀必须是真桶,而不是别的债

`post-v032-evaluation.md` 里排在「更后 / 有人要」的条目,现在仍然不该插队:

| 候选 | 现在做? | 原因 |
|------|---------|------|
| **R2/S3 真桶** | **做,这就是 v0.3.5** | 白名单已经放行 host;用户配了会以为能用。没有实测就不能写「已支持」 |
| snapshot list CLI | 否 | 工具半成品,不挡 RPO=0 |
| doctor 查 pi `auth.json` 有无 | 否 | 小,但不是存储路线 |
| `distributed-deployment.md` 的 `pkill` | 否(可另开文档 PR) | 运维口径,不挡存储 |
| OTEL / celld 桶 prefix | 否 | v0.2 利用评审已标 P2 |
| GCS 会话客户端 | 否 | 新客户端,不是换 endpoint |
| rename `bos.js` | 否 | 符号爆破,无用户收益 |
| provider 认证 / 快照 TUI / 会话 merge | **永不排进近版** | 做错抽象或撞红线 |

唯一会让「用户配了 R2 却丢会话」再次发生的缺口,是 **从未在真 R2 上跑过 aws CLI 条件写 + celld `--endpoint`**。门禁只能在「已经能说话」的存储上拒绝不合格者;它不能证明 R2 的 ETag/校验和/region=`auto` 与当前 `bos.js` 合拍。

## 3. 真桶实测会碰到的实现摩擦(先记下来,不要预写)

代码路径已经参数化(`storeFromCfg`、`--endpoint`、`--region`、非 BOS 不 `create-bucket`)。仍可能在真 R2 上炸,需要 **测到再改**,不要猜着补:

1. **AWS CLI v2 默认 checksum**:较新 aws CLI 会给 PutObject 加校验和头,R2 有时 400。若出现,再在 `awsEnv` 设 `AWS_REQUEST_CHECKSUM_CALCULATION=when_required`(或等价),并证明 BOS 默认路径不受影响。
2. **ETag 引号 / 弱 ETag**:探针已要求有 etag 且错误 If-Match 必须 conflict。R2 若返回形式不同导致「正确 etag 也 412」,再改 `etagFromGetStdout`,不要事先改。
3. **path-style vs virtual-hosted**:自定义 `--endpoint-url` 时 aws CLI 一般走 path-style;celld `object_store` 自己选。两边必须打 **同一 bucket**。
4. **profile 不要复用 `[bos]`**:R2 应用独立 profile(如 `[r2]`)或成对 env AK/SK。用百度 AK 打 R2 host 会得到签名错误,不是 CAS 失败。
5. **桶必须已存在**:`setup.sh` 对非 BOS 拒绝 `create-bucket`(R2 控制台建桶)。实测步骤从「已有空桶」开始。
6. **own.json 清理**:`ensureCelld` 只对 `celagent-*` 前缀桶默认清残留。R2 测试桶应用这个前缀,或显式 `CELAGENT_CLEAN_OWN=1`(共享桶不要开)。

这些都不是「先实现再找桶」。没有桶就没有这些 diff。

## 4. 「已测」和「已支持」怎么写(红线)

实测通过之后,文档允许写:

- 「Cloudflare R2:**已实测** 会话 CAS + 跨进程恢复 + celld 双节点 health(日期 + 不含密钥的 endpoint 形态)」。

不允许写:

- 「支持所有 S3 兼容存储」
- 「已支持 MinIO / B2 / Spaces」
- 未跑过的 AWS S3 / Tigris 写成与 R2 同级「已支持」(可留「候选,未测」+ settings 样例)

默认后端 **仍然是 BOS**。不改 `[bos]` 缺省 profile,不改无 `persistence.endpoint` 时的行为。

## 5. 建议的验收剧本(有凭证时)

优先 **R2**(celld 官方 release 测的就是它)。S3 作为并列替代,二选一即可,不要两个都做才发版。

前置:桶已建;独立 profile 或成对 env;本机有 `aws`、`celld`、源码树。

1. `persistence.endpoint=https://<account>.r2.cloudflarestorage.com`,`region=auto`,`profile=<非 bos>`,`bucket=celagent-*`。
2. `celagent cas-probe` 退出 0。
3. 不依赖真实 LLM:向 `sessions/<id>.json` 写入一轮(TUI mock 或直接 `bosPut`),另一进程 `celagent export <id>`(或 `celagent <id>` 能 load)看到同一内容。
4. `ensureCelld` / `node_mgr.sh start`:18090+18091 `/__celld/health` 为 ok;桶内出现 `cells/` 或 `nodes/`(证明执行层也打到该 endpoint)。
5. `celagent doctor` 六步核心通过(Celld 离线可「核心正常」,但本剧本应有节点)。
6. 把日期、后端名、**不含密钥** 的 endpoint 形态写入 `docs/s3-compat-evaluation.md` 表格「现状」列。README 增加 R2 样例,标明已测。

失败则:修 `bos.js`/`awsEnv`/文档,重跑;不要降低验收。

## 6. 阻塞与非阻塞

**阻塞实现与发版**:R2 或 S3 的 API 凭证 + 已存在的测试桶。本评估环境没有。

**不阻塞评估**:本文 + `docs/v035-scope.md` 可以先合进 main。

**不要做的「替代发版」**:

- 只改文档、把「可配置」写成「已支持」
- 在 CI 装 MinIO 打绿
- 把 snapshot list、pkill 文档、doctor 查 key 有无凑成 v0.3.5

若长期没有凭证:保持 Latest = v0.3.4,下一刀继续等;或用户明确改下一刀主题(那就不是存储 P1)。

## 7. 明确不做

- 本评估 PR 改 `src/` `bin/` `scripts/` 行为
- 无凭证时编造 R2 测试记录
- 为 R2 预加 checksum 补丁(未复现就改,可能伤 BOS)
- GCS / Azure / MinIO / 改默认 endpoint / 自研认证

## 8. 源码索引(实测时看这些)

| 路径 | 真桶时会碰到 |
|------|----------------|
| `src/bos.js` `probeStoreCas` / `awsEnv` / `bosPut` | CAS、checksum、ETag |
| `bin/celagent-tui.mjs` `storeFromCfg` / `ensureCelld` / `persistTurnToBos` | region=`auto`、profile、拉起 celld |
| `setup.sh` | 非 BOS 不建桶;建完跑 `cas-probe` |
| `scripts/store_env.sh` | 脚本与 TUI 同一份 endpoint/region/profile |
| `scripts/celld-store-test.sh` | 有节点时的执行层回归(可对 R2 跑,失败则记,不降低会话验收) |
| `README.md` / `docs/s3-compat-evaluation.md` | 实测后改叙事 |
