# v0.3.4 范围(本刀实现)

> 版本号 **`0.3.4`**。依据 `docs/s3-compat-evaluation.md` P1 + HANDOFF「下一刀」。
> **本文件是实现合同**。默认仍是 BOS;无 persistence.endpoint 的用户应只多一次 doctor/setup 的 CAS 探针。
> Latest 在发版前仍是 v0.3.3。不移动 v0.3.0/v0.3.1/v0.3.2/v0.3.3。

## 用户能感知什么

- `celagent doctor` 增加 **CAS** 步:对当前 bucket 做 If-None-Match / If-Match / 写后读。失败则非零退出,文案含 **RPO=0**。
- `celagent cas-probe` 只跑这一项(setup.sh 在建桶后调用)。
- 会话权威写入前先做一次探针;CAS 不合格则 **拒绝写 sessions/**,不把历史交给会静默覆盖的存储。
- worker 不再带未调用的手写 SigV4。产物路径仍是 webhook。

本版 **不宣称**「已支持 R2/S3」。本环境无非 BOS 凭证,不做真桶联调。v0.3.5 是正确性修复;真桶实测是 **v0.3.6**。

## 做

### 1. `src/bos.js` — CAS 探针

| 符号 | 行为 |
|------|------|
| `bosDelete` | `s3api delete-object`;非法 endpoint 同 put/get |
| `evaluateCasChecks` | 纯函数:忽略条件写 → `cas-ignored`;缺 ETag / 写后读失败 → 对应错误码 |
| `probeStoreCas` | 写 `celagent-cas-probe/<uuid>.json`,跑四步,`finally` 删除。可注入 `ops.put/get/del` 供无 aws 的 CI |

探针步骤:

1. 无条件 PUT
2. GET 立即看到内容且有 ETag
3. 已存在 + `If-None-Match: *` → 必须 conflict,成功则视为忽略条件
4. `If-Match: 错误 etag` → 必须 conflict
5. `If-Match: 正确 etag` → 必须成功,GET 看到新内容

### 2. TUI / setup 门禁

- `doctor` 变为 6 步,最后一步 CAS;缺凭证/不通则跳过并保持失败。
- `persistTurnToBos` 进程内缓存一次探针,失败则 `warnOnce` 后 return。
- `setup.sh` 桶可用后跑 `celagent cas-probe`(或源码 `bin/celagent-tui.mjs cas-probe`),失败则退出。

### 3. 运维脚本

- `scripts/celld-store-test.sh` → 转调 `celld-bos-test.sh`(配置已来自 settings)。
- `celld-bos-test.sh` 增加会话路径 `cas-probe` 检查(有 node/源码时)。

### 4. 删死代码

`worker/src/index.js` 未调用的 `bosPut`/`bosGet` 及 HMAC/SigV4 辅助函数全部删除。保留 `bosPutProxy`。

## 明确不进 v0.3.4

- 真 R2/S3/Tigris 联调与「已支持」矩阵
- GCS 客户端、rename `bos.js`、provider 认证、快照 TUI、会话合并
- 每次 turn 都重新探针(只在进程内做一次)
- 用 CI 假绿代替有凭证机器上的 BOS 回归

## 验收

- [ ] 内存 store **遵守** CAS → `probeStoreCas` ok
- [ ] 内存 store **忽略** If-Match → `error === "cas-ignored"`,文案含 RPO
- [ ] doctor 源码含 CAS 步与 `cas-probe` 命令
- [ ] worker 不再有 `async function bosPut(` / `hmacSha256Raw`
- [ ] `npm test` 全绿;`bash -n` 通过
- [ ] 版本号 0.3.4(package.json / TUI / install.sh)

## 发版

实现合进 `main` 后打 tag **`v0.3.4`**(不移动旧 tag)。随包 celld 仍为 v0.2.0。
