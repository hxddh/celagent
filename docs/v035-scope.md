# v0.3.5 范围(下一刀实现)

> 版本号 **`0.3.5`**。依据 `docs/post-v034-evaluation.md`。
> **本文件是实现合同**。默认仍是 BOS;`[bos]` / `bj` / `s3.bj.bcebos.com` 用户应无感。
> Latest 在发版前仍是 v0.3.4。不移动旧 tag。
> **无真桶凭证则不要开实现 PR,更不要发版。**

## 用户能感知什么

- 配了 **已实测** 的那一种非 BOS 后端(合同指定优先 R2,否则 S3)后:`cas-probe` 通过、写一轮会话、另一进程能恢复。
- README / 存储评估表格把该后端标成 **已实测**,其它合格后端仍是「候选,未测」。
- **不**把「S3 兼容」写成支持声明。未测的 Tigris/S3(若实测的是 R2)保持候选。

## 做

### 1. 真桶剧本(验收,不是可选)

后端二选一,优先 R2:

| | R2 | AWS S3 |
|--|----|--------|
| endpoint | `https://<account>.r2.cloudflarestorage.com` | `https://s3.<region>.amazonaws.com` 或 `https://s3.amazonaws.com` |
| region | `auto` | 桶所在 region |
| profile | 独立 profile(不要 `[bos]`)或成对 env AK/SK | 同左 |
| 桶 | 控制台预建,建议 `celagent-*` 前缀 | 预建;本版 setup **不**对非 BOS `create-bucket` |

必须留下 **可复核记录**(日期、后端名、endpoint **形态**、命令与退出码)。禁止写真实 AK/SK、account id 可在文档用 `<account>` 占位。

步骤:

1. `celagent cas-probe` 退出 0
2. 写入 `sessions/<id>.json` 一轮(TUI mock 或 `bosPut`);另一进程 `celagent export <id>` 内容一致
3. 双节点 health 200;桶内可见 `cells/` 或 `nodes/`(执行层打到同一 endpoint)
4. `celagent doctor` 在有凭证+桶时 CAS 步为 ✓

### 2. 只在真桶暴露问题时改代码

允许的修复(测到再动):

- `awsEnv` 为 R2 需要的 CLI checksum 行为,且 BOS 回归不坏
- ETag 解析(引号/弱 ETag)导致的假 412
- 文档里 R2 `region=auto`、独立 profile、桶须预建

禁止:没复现就改默认 aws 参数;为 MinIO 开洞;改 BOS 缺省。

### 3. 文档叙事(仅在第 1 项通过后)

- `docs/s3-compat-evaluation.md` §3.1「现状」列:实测后端改为已测,并注明日期
- `README.md`:对象存储权威源;BOS 仍是默认例子;增加已测后端的 settings 样例
- `HANDOFF.md`:默认后端仍写 BOS;可写「R2 已实测(非默认)」

## 明确不进 v0.3.5

- 无凭证的「文档-only」发版
- CI MinIO/LocalStack/内存 store 当作非 BOS 实测
- Tigris 第二后端(除非 R2 与 S3 都拿不到、用户改指定)
- GCS、Azure、rename `bos.js`、桶 prefix、OTEL
- snapshot list CLI、provider 认证、快照 TUI、会话合并、自动删 `sessions/`
- 把 wrangler 空 token 改 fail-closed

## 验收

- [ ] 真桶 `cas-probe` 退出 0(不是内存 ops)
- [ ] 跨进程恢复同一 session id
- [ ] celld 双节点 health + 桶内执行层前缀有对象
- [ ] 文档「已实测」只覆盖实际跑过的那一个后端
- [ ] `npm test` 全绿(BOS 用户路径无回归)
- [ ] 版本号 0.3.5

## 发版

实现合进 `main` 后打 tag **`v0.3.5`**(不移动旧 tag)。随包 celld 仍为 v0.2.0。
