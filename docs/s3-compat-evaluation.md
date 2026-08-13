# celagent × S3 兼容对象存储 — 架构评估与迭代计划

> 评估对象:把数据层从「BOS 专用」扩到「所有兼容 S3 的对象存储」。
> 依据:celagent 源码对照、[celld 官方文档](https://celld.dev/docs)(Configure object storage + Ownership and fencing)、`docs/bos-compat.md` 实测、`docs/celld-v02-evaluation.md`。
> **本 PR 只评估,不改运行时代码。**

## 0. 结论先行

**不能、也不该无条件支持「所有 S3 兼容存储」。**

celagent 的卖点是 **会话永不丢(RPO=0)** + 多进程 CAS 不互相覆盖。这依赖对象存储的三个原语,不是「能 PUT/GET」就够。celld 官方写得很硬:

> The store must provide conditional writes and read-after-write consistency.
> Amazon S3, Cloudflare R2, Google Cloud Storage, Azure Blob Storage, and Tigris qualify;
> MinIO (community edition), Backblaze B2, Hetzner, and DigitalOcean Spaces do not.
> celld is not correct on such a store: two nodes can then own one cell.

正确目标不是「换个 endpoint 就能接 MinIO/B2」。正确目标是:

1. **把 BOS 从一个写死的实现,变成默认且已实测的一个合格后端**;
2. **让其它合格后端(AWS S3 / Cloudflare R2 / Tigris)可配置接入**;
3. **不合格后端拒绝宣称 RPO=0**(doctor 门禁失败即失败),而不是静默写进去。

分阶段:先解耦配置与静默回退,再做 CAS 探测,再扩第一批合格后端。GCS 是 celld 一等公民,但 **不是** celagent 会话直写路径的一等公民(见 §3.2)。

## 1. 两套存储消费者,通常共用一个 bucket

```
TUI / bos.js / bos-tools     ──aws s3api──▶  sessions/*.json
                                            snapshots/*.json     ← 会话权威 (RPO=0)
setup / node_mgr / cluster
ensureCelld                  ──celld CLI──▶  cells/*/ltx
                                            nodes/  fleet/  wake/
                                            deploy/              ← 执行层 (celld 自己的 RPO)
worker obj-put / 任务产物    ──webhook──▶    workspace/*         ← 非权威,worker 零凭证
```

| 消费者 | 协议 | 条件写 | 失败后果 |
|--------|------|--------|----------|
| **会话权威**(`src/bos.js`) | aws CLI `s3api` + `--if-match` / `--if-none-match` | 必须 | 并发覆盖会话、首写互踩、RPO=0 作废 |
| **执行层**(celld) | Rust `object_store`;S3 方言或 `gs://` XML | 必须 | 双 owner、历史分叉、接管读到未 ACK 的尾 |

`worker/src/index.js` 里的 `bosPut`/`bosGet`(手写 virtual-hosted SigV4,host 绑 `s3.bj.bcebos.com`) **没有任何调用点**;`obj-put` 与任务产物走 `bosPutProxy` → 本地 webhook。这是死代码,不是第三条存储消费者,扩后端时不要去适配它(删掉即可,见 `docs/post-v032-evaluation.md`)。

架构分层 **仍然成立**,而且扩后端时更该守住:

- 会话 JSON 继续走 celagent 直写,不依赖节点,不塞进 cell SQLite。
- celld 继续只当执行层。它已经能说多种存储,但 **celagent 的拉起/运维脚本还不会**。
- 两套消费者应对 **同一合格 bucket**(celld 可用 key prefix 自隔离,会话仍可在桶根 `sessions/`)。不要做成「会话在 MinIO、celld 在 R2」——运维面翻倍,跨机恢复心智碎掉。

## 2. 桶必须提供什么(门禁,不是清单)

celld [Ownership and fencing](https://github.com/denoland/celld/blob/main/docs/fencing.md) 要求三条;celagent 会话权威 **同一套**:

| # | 原语 | HTTP / S3 | celagent 用法 | celld 用法 |
|---|------|-----------|---------------|------------|
| 1 | 条件创建 | `If-None-Match: *` | 冷启动首写(Bug 76) | own.json / `e.seal.json` 首写 |
| 2 | 条件覆盖 | `If-Match: <etag>` | 续写乐观锁,412 重读重试 | own.json CAS 换 owner |
| 3 | 写后读一致 | PUT 成功后 GET 立即看到 | 恢复 BOS-first;合并轮次 | ACK 前再读 ownership;lease 发现 |

附加(执行层更敏感,会话路径也用):

- **LIST 足够新**:`nodes/`、`sessions/` 列表。最终一致 LIST 会让 `celagent list` / 集群发现短暂漏节点,通常可接受;lease 误判离线则不能接受长时间陈旧。
- **ETag 稳定且可回传**:条件覆盖比的是 ETag,不是内容哈希约定。有的兼容层返回弱 ETag 或改写引号,aws CLI 会签失败或 CAS 永远 412。
- **接受条件头且真正执行条件**:最危险的失败模式是 **接受 `If-Match` 但忽略它**(fencing 原文:fails late and silently)。必须用探测,不能看「PUT 200」。

会话权威 **不**需要 multipart、不需要目录、不需要跨对象事务。单 key 原子 PUT 足够。

## 3. 哪些后端能当权威源

分级以 **celld 官方资格 + celagent 实测 + 会话客户端能否说同一种方言** 为准。未在本机复测的标「未测」。

### 3.1 合格(可作为权威源候选)

| 后端 | celld | 会话直写(aws CLI) | 现状 | 备注 |
|------|-------|-------------------|------|------|
| **百度 BOS** | 官方名单无;本仓库 **实测 CAS/LIST/lease 全过** | ✅ `s3.<region>.bcebos.com` | **唯一生产默认** | SigV4 与 boto3 有差异,必须走 aws CLI + `AWS_PROFILE`(见 bos-compat §二.2–3) |
| **Amazon S3** | ✅ 官方合格 | ✅ 原生;需 path/virtual 与 region | 未测 | PutObject 条件写已是官方能力;创建桶/权限模型与 BOS 不同 |
| **Cloudflare R2** | ✅ 官方合格;celld release 测的就是 R2 | ✅ S3 API,`region=auto` | 未测 | 文档示例:`S3_ENDPOINT=https://<ACCOUNT>.r2.cloudflarestorage.com` |
| **Tigris** | ✅ 官方合格 | ✅ S3 API | 未测 | 全球强一致定位与 CAS 匹配 |

### 3.2 执行层合格、会话层不能直接复用 aws CLI

| 后端 | celld | 会话直写 | 结论 |
|------|-------|----------|------|
| **GCS (`gs://`)** | ✅ 一等;`x-goog-if-generation-match` + ADC | ❌ GCS **不对 PUT 应用 S3 `If-Match`**(fencing 原文)。aws CLI `--endpoint-url` 走 XML 互操作 **不能**当 CAS | **P2**:要接 GCS,必须给会话路径单独写 generation 客户端,不能「换 endpoint」 |
| **Azure Blob** | 官方「存储属性合格」 | ❌ celagent 无 Azure 客户端 | **不做**。celld 若未把 Azure 做成与 `gs://` 同级 URI,celagent 更不应承诺 |

### 3.3 不合格(禁止当权威源)

| 后端 | 原因 |
|------|------|
| **MinIO 社区版** | 官方:不实现所需条件写 → 双 owner |
| **Backblaze B2** | 同上 |
| **Hetzner Object Storage** | 同上 |
| **DigitalOcean Spaces** | 同上 |

本机 `http://127.0.0.1:9000`(当前白名单已放行)只适合 **冒烟/开发假存储**,doctor 仍应跑 CAS 探测并打印「非 RPO=0」。不要把 MinIO 写进 README 当部署选项。

**阿里云 OSS / 腾讯 COS / 其它「S3 兼容」**:默认归入「未证明」。必须先过 §7 的 CAS 探针,再考虑加白名单。营销页写 S3 兼容 ≠ 执行 `If-Match`。

## 4. 现状:BOS 写死在哪(爆破半径)

配置表面上有 `persistence.endpoint`,实际默认链路几乎处处假设 BOS。

### 4.1 运行时

| 位置 | 写死什么 | 影响 |
|------|----------|------|
| `src/bos.js` `EP` | `https://s3.bj.bcebos.com` | 缺省/非法 endpoint **静默退回 BOS** |
| `isAllowedEndpoint` | 仅 `s3.*.bcebos.com` + 环回;其它要 `CELAGENT_ALLOW_ENDPOINT=1` | 用户设 R2 URL 若不设 env,写入打到百度 |
| `awsEnv` | 无完整 AK/SK 时强制 `AWS_PROFILE=bos` | 标准 `default`/`r2` profile 用不上 |
| `bin/celagent-tui.mjs` `ensureCelld` | `--region` 默认 `bj`;凭证探测 `--profile bos` | celld 签名 region 错则全挂 |
| `config set persistence.endpoint` | 拒绝非 BOS https | 合法合格后端配不进去(除非绕 env) |
| `doctor` | 只查 `[bos]` profile | R2/S3 用户永远「无凭证」 |
| `src/bos-tools.js` | 经 `resolveEndpoint` | 记忆工具跟着静默回退 |
| `worker/src/index.js` `bosPut/bosGet` | host=`${bucket}.s3.bj.bcebos.com`,region=`bj` | **死代码,无调用**;产物走 `bosPutProxy` |

### 4.2 运维脚本(全部忽略 settings 里的 endpoint)

`setup.sh`、`install.sh`、`scripts/node_mgr.sh`、`scripts/cluster_mgr.sh`、`scripts/celld-bos-test.sh`:

- endpoint 常量 `https://s3.bj.bcebos.com`
- `export AWS_PROFILE=bos AWS_REGION=bj`
- `celld --endpoint … --region bj`
- 凭证检测:`aws configure get … --profile bos`
- `create-bucket --region bj`(R2/S3 行为不同,不能照抄)

TUI 的 `ensureCelld` 已读 `cfg.persistence.endpoint`(再经 `resolveEndpoint` 过滤);**脚本不读**,所以「config set 了 endpoint」和「node_mgr start」会指向两个存储。

### 4.3 文档与产品叙事

HANDOFF / architecture / README / distributed-deployment 一律「BOS 权威源」。扩展点 2 已承认「BOS 为本项目唯一实测后端」,但没写清 **合格/不合格** 分界,也没写静默回退是缺陷。

## 5. 最危险的现状:静默退回 BOS

```211:218:tests/review-logic-proofs.test.mjs
test("resolveEndpoint 拒绝非 BOS https", async () => {
  const { resolveEndpoint, isAllowedEndpoint } = await import("../src/bos.js");
  // ...
  assert.equal(resolveEndpoint("https://evil.example"), "https://s3.bj.bcebos.com");
```

安全动机成立:任意 endpoint 可变成 SSRF/凭证泄漏面,所以 v0.3.1 加了白名单。

实现选择不成立于「多后端」目标:

- 用户把 endpoint 设成 R2,没设 `CELAGENT_ALLOW_ENDPOINT=1` → **会话写到百度 BOS**,无错误。
- 测试把「拒绝」编码成「改写为 BOS」,后续改成 fail-closed 必须同步改 proof。

**迭代第一刀必须把「拒绝」改成失败,而不是改写成默认 BOS。** 白名单可以保留;不允许就 `throw` / CLI 退出,不要 PUT 到另一个云。

## 6. ADR 草案(扩后端时遵守)

| # | 决策 | 理由 | 不做 |
|---|------|------|------|
| A | **合格后端白名单 + CAS 探测**,不是「S3 兼容即可」 | 忽略条件头的存储会静默双写 | 把 MinIO/B2 写进支持矩阵 |
| B | **会话权威与 celld 共用同一桶**(可选 key prefix) | 跨机恢复一个心智;凭证一份 | 会话 MinIO + celld R2 |
| C | **会话路径继续 aws CLI**(S3 方言);GCS 另开客户端 | BOS/S3/R2/Tigris 一条路径;GCS 不认 PUT If-Match | 用 boto3(BOS 已证明签名失败) |
| D | **配置单一来源**:`persistence.{bucket,endpoint,region,profile}` | 脚本与 TUI 不再各写各的 | 环境变量与 settings 再发明一套平行配置 |
| E | **默认仍是 BOS** | 现有用户/凭证/`[bos]` profile 零迁移 | 为了「通用」改默认 endpoint |
| F | **不把 worker 手写 SigV4 当存储入口** | 函数无调用点;产物已走 webhook | 为死代码做多云适配 |
| G | **celld `--bucket` 继续 `s3://`**,直到做 GCS | 与当前 spawn 一致 | 本阶段引入 `gs://`(会话层接不住) |
| H | **改名可滞后**:文件仍叫 `bos.js`,对外叙事改成「对象存储权威源」 | 重命名爆破 tests/exports/文档 | 评估阶段就全局 rename |

## 7. 分阶段迭代计划

### P0 — 解耦,行为对现有 BOS 用户不变(建议下一刀)

目标:合格后端 **配得进去、脚本听配置、配错了失败而不是写到百度**。

1. **`resolveEndpoint` fail-closed**:非法/空 override 不返回 `EP`;调用方报错。无 override 时默认仍 BOS。
2. **`awsEnv` 读 `persistence.profile`**,缺省 `bos`。完整 `AWS_ACCESS_KEY_ID`+`SK` 仍优先且不混用。
3. **region 缺省**:仅当 endpoint 是 `*.bcebos.com` 时默认 `bj`;否则要求显式 `persistence.region`(R2=`auto`)。
4. **白名单扩展**(https only,环回例外):
   - 现有 `s3.*.bcebos.com`
   - `s3.<region>.amazonaws.com` / `s3.amazonaws.com`
   - `*.r2.cloudflarestorage.com`
   - `*.tigris.dev` / 文档公布的 Tigris host
   - 其它仍要 `CELAGENT_ALLOW_ENDPOINT=1`(escape hatch),但 **不再静默改写**
5. **脚本从 settings.json 读** `bucket/endpoint/region/profile`,去掉硬编码 EP。`create-bucket` 对非 BOS 改为「桶必须已存在」(R2 常在控制台建),BOS 路径保持现逻辑。
6. **doctor**:按 profile/env 查凭证;打印实际 endpoint/region;发现「配置了非 BOS 但 resolve 会回退」这类状态(P0 落地后不应再出现)。
7. **proof 测试**:`resolveEndpoint("https://evil.example")` 不再等于 BOS URL;改为断言拒绝。

不在 P0:接 R2 实测、rename `bos.js`、GCS。worker 手写 SigV4 是死代码,扩后端不必碰。

### P1 — CAS 门禁 + 第一批合格后端文档(需真桶)

1. **`celagent doctor` / `setup` 增加 CAS 探针**(从 `celld-bos-test.sh` 抽公共函数):
   - `If-None-Match: *` 对已存在对象 → 必须 412
   - `If-Match: 错误 etag` → 必须 412
   - `If-Match: 正确 etag` → 必须成功
   - PUT 后立即 GET 看到新内容
   - 失败则 **拒绝启动权威写入**,文案写清「此存储不能保证 RPO=0」
2. 参数化 `scripts/celld-bos-test.sh` → `scripts/celld-store-test.sh`(保留旧名 wrapper)。endpoint/profile/region 来自 settings。
3. **文档化第一批**:BOS(默认,已测)、AWS S3、R2、Tigris 的 settings 样例与凭证。README 心智模型改为「对象存储权威源」,例子仍用 BOS。
4. **在 R2 或 S3 上跑一遍** 会话 CAS + `ensureCelld` + 双节点 health。未跑过的后端保持「候选,未测」,不写「已支持」。
5. 删掉 worker 未调用的 `bosPut`/`bosGet`(死代码),不要参数化那条路径。

### P2 — GCS / prefix / 叙事收口

1. **GCS**:仅当明确要做。会话客户端走 generation precondition,与 celld `gs://` 对齐;spawn `--bucket gs://…`,无 `S3_ENDPOINT`。工作量是新客户端,不是配置项。
2. **桶 prefix**(`s3://bucket/celagent`):celld v0.2 已支持,且 **只影响 celld 自己的对象**(cells/nodes/deploy/…)。会话权威继续读桶根 `sessions/*`,两套消费者从不互相 LIST。新安装可给 celld 加 prefix 做 fleet 隔离,**不必搬迁已有会话对象**。不要把「同桶」理解成「同一 key 前缀」。
3. 文件/符号:`bos.js` → `store.js`、`queueBosWrite` 别名;package exports 过渡期双路径。
4. 明确 **不做** Azure、MinIO、B2、Spaces。

### 建议版本切分

| 版本 | 内容 |
|------|------|
| 本评估(无版本号) | 本文 + 交叉引用 |
| **v0.3.3**(下一刀,合同见 `docs/v033-scope.md`) | fail-closed、配置单一来源、脚本读 settings、白名单扩合格 host |
| **v0.3.4** | CAS doctor、R2/S3 至少一种实测、文档改叙事;可删 worker SigV4 死代码 |
| 更后 | GCS / prefix / rename |

P0 可独立发版:BOS 用户无感,只修「配了别的云却写到百度」这个缺陷。

## 8. 明确不做

- 把「S3 兼容」写成支持声明。
- 为了通用性改掉 BOS 默认、强迫现有用户换 profile 名。
- 会话权威改走 celld SQLite(与 v0.2 评估结论冲突:节点可丢,会话不能丢)。
- 用 SDK 替换 aws CLI 作为 BOS/S3 主路径(boto3 已在 BOS 上 SignatureDoesNotMatch)。
- 推广或参数化 worker 内未调用的手写 SigV4。
- 在未做 CAS 探测前把 MinIO 本地当成 HA 部署。
- 本评估 PR 改 `src/` `bin/` `scripts/` 行为(避免评估与实现缠在一起)。

## 9. 验收(将来落地时)

P0:

- [ ] `config set persistence.endpoint https://evil.example` → 非零退出,settings 不变,无 BOS PUT
- [ ] 未设 endpoint 时行为与现在相同(BOS + `[bos]` + `bj`)
- [ ] `node_mgr.sh` / `cluster_mgr.sh` / `setup.sh` 使用 settings 中的 endpoint/region/profile
- [ ] `npm test` proof 更新后全绿

P1:

- [ ] doctor 在缺 CAS 的存储上失败,文案含 RPO
- [ ] 至少一种非 BOS 合格后端(建议 R2,与 celld CI 对齐)完成:建配置 → 写一轮会话 → 另一进程 `celagent <id>` 恢复
- [ ] `celld-store-test.sh` 对 BOS 回归 0 失败(有凭证的机器)

## 10. 源码索引

| 文件 | 与存储耦合 |
|------|------------|
| `src/bos.js` | 默认 EP、白名单、profile=`bos`、CAS PUT/GET |
| `src/bos-tools.js` | list/get/put 经 resolveEndpoint |
| `bin/celagent-tui.mjs` | ensureCelld region、config 白名单、doctor 凭证、list/export/rm |
| `worker/src/index.js` | `bosPut`/`bosGet` 死代码(无调用);产物走 webhook |
| `setup.sh` `install.sh` | 凭证、建桶、deploy、写 settings |
| `scripts/node_mgr.sh` `cluster_mgr.sh` | 启动 celld、列 nodes/ |
| `scripts/celld-bos-test.sh` | 17 项 BOS CAS/lease 探针(P1 应参数化) |
| `tests/review-logic-proofs.test.mjs` | 把静默回退编码成正确行为 |
| `docs/bos-compat.md` | BOS 实测边界,扩后端后仍有效 |
| `docs/architecture.md` §3 扩展点 2 | 当前「换存储」入口,过时 |

celld 侧不需要等上游:v0.2 已接 S3 兼容 endpoint 与 `gs://`。卡住的是 **celagent 自己的配置、脚本、白名单和会话客户端方言**。
