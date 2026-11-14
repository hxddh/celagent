# v0.3.5 范围(本刀实现)

> 版本号 **`0.3.5`**。对照 v0.3.4 源码挖出的 P0/P1/P2 正确性修复。
> **本文件是实现合同**。默认仍是 BOS。**不宣称**已支持 R2/S3。真桶实测现为 **v0.3.8**(中间插入了 v0.3.6 审查修复与 v0.3.7 persist 主路径)。
> 不移动 v0.3.0–v0.3.4。

## 用户能感知什么

- 配了 `persistence.region`(如 R2 `auto`)后,会话读写 / `cas-probe` / export / rm 与 doctor LIST 用同一套 `AWS_REGION`。
- 网络或凭证导致 CAS 探针失败时,下一轮 persist 会重试;只有存储真的忽略 If-Match 才整场拒绝写入。
- `celagent rm` 失败不再打印「已删除」。
- `celagent list` / `history_search` 在 aws 失败时报错,不再假装没有会话。
- `install.sh` 装完会跑 `cas-probe`;非法 endpoint 在脚本侧也 fail-closed。
- `session_snapshot` 带上 `content`/`toolResults`;`history_search` 能搜到 `snapshots/`。

## 做

### P0

- `runAws` / `bosPut` / `bosGet` / `bosDelete` / `probeStoreCas` 接受 `region`,写入 `AWS_REGION`。
- `casGateSticky`:仅 `ok` 或 `cas-ignored` 粘滞;其它失败下次重试。
- `celagent rm` 走 `bosDelete`,检查 `ok`。

### P1

- `awsJson`:失败返回 `{ok:false,error}`,不解析成 `[]`。
- `persistenceFromCfg`:非法 endpoint → `endpoint-not-allowed`,不是「未配置 bucket」。
- doctor 非法 endpoint 步号 `[1/6]`。
- `install.sh` 写配置后 `cas-probe`;`store_env.sh` endpoint 白名单 fail-closed。

### P2

- 快照缓存与 PUT 含 `content` / `toolResults` / `session`。
- `history_search` 读 `snapshots/`。
- worker `delegate` 内部 `stub.fetch` 转发 `X-Celagent-Token`。

## 明确不进 v0.3.5

- 真 R2/S3 联调与「已支持」矩阵
- 把 CI 内存探针当成非 BOS 实测
- provider 认证 / 快照 TUI / 会话合并

## 验收

- [x] `casGateSticky` 仅 ok / cas-ignored
- [x] `persistenceFromCfg` 非法 endpoint 不伪装成 no-bucket
- [x] `probeStoreCas` 把 region 传给 ops
- [x] `npm test` 全绿;`bash -n` 通过
- [x] 版本号 0.3.5(package.json / TUI / install.sh)

## 发版

实现合进 `main` 后打 tag **`v0.3.5`**(不移动旧 tag)。随包 celld 仍为 v0.2.0。
