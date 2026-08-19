# v0.3.6 范围(本刀实现)

> 版本号 **`0.3.6`**。对 v0.3.4→v0.3.5 变更集深度代码审查挖出的 9 项正确性/一致性修复(PR #16)。
> **本文件是实现合同**。默认仍是 BOS。**不宣称**已支持 R2/S3。
> 原定 v0.3.6 的非 BOS 真桶实测**顺延为 v0.3.7**,后又顺延 **v0.3.8**(v0.3.7 先修 persist 主路径)。不移动 v0.3.0–v0.3.5。

## 用户能感知什么

- 探针遇网络抖动时,本轮写入**留在队列重试**(指数退避,上限 60s),恢复后按序补写——不再静默丢一轮。
- 存储真的不合格(no-etag / 条件头被忽略 / 写后读不一致)时判决**一次粘滞**,不再每轮重跑探针再丢轮。
- 中途 `config set persistence.bucket/endpoint` 换存储后,CAS 判决按新 store 重新探测,旧判决不沿用。
- `install.sh` / `setup.sh` 不再把一次网络超时误报成「此存储不能保证 RPO=0」——`cas-probe` 以 exit 2 区分「探针未完成,请重试」。
- IPv6 回环 endpoint(如 `http://[::1]:9000` 的 MinIO)在 bash 与 JS 门禁两侧都能正常放行。
- `history_search` 命中片段一定包含命中文本;会话对象读失败不再吞掉快照命中。
- 长会话进程内存不再随全量 content/toolResults 无界增长。

## 做

### P0(CAS 门禁 / RPO=0)

- `evaluateCasChecks`:区分 transient(`transient:true`,探针未完成、无法判定)与结论性能力失败;NotImplemented/501 判结论性。
- `casGateSticky`:结论性失败(no-etag / if-none-match / if-match-wrong / read-after-write / cas-ignored)也粘滞;transient 不粘滞。
- TUI 门禁缓存按 `endpoint|bucket|profile|region` 键控。
- `persistTurnToBos` transient 时返回重试信号;`pumpBosQueue` 任务留队首、指数退避;退出 flush 统一 10s 上限。
- `cas-probe` transient 退出码 2;install.sh / setup.sh 按退出码区分提示。

### P1(endpoint 白名单三处对齐)

- bash / JS 两侧修 IPv6 方括号解析死代码。
- `s3.*.bcebos.com` / `s3.*.amazonaws.com` 中段收紧为单标签 `[a-z0-9-]+`,与 JS 正则一致。
- install.sh 优先 source `scripts/store_env.sh`,仅独立分发用同步回退拷贝(实拷贝 3 → 2)。

### P2(记忆工具 / 内存)

- `history_search`:先扫完 snapshots/ 再决定;片段取匹配位置附近窗口。
- `snapshotTurns` 进程内只留摘要;`session_snapshot` 从 BOS 权威源重建全量,内存摘要补齐未刷轮。

## 明确不进 v0.3.6

- 真 R2/S3 联调与「已支持」矩阵(→ v0.3.7)
- provider 认证 / 快照 TUI / 会话合并

## 验收

- [x] `evaluateCasChecks` transient / 结论性分类回归
- [x] `casGateSticky` 新粘滞语义回归
- [x] bash 白名单 IPv6 + 多标签 host 回归;JS 侧同断言
- [x] 片段含命中文本回归
- [x] `npm test` 全绿;`bash -n` 通过
- [x] 版本号 0.3.6(package.json / TUI / install.sh)

## 发版

实现合进 `main`(PR #16)后打 tag **`v0.3.6`**(不移动旧 tag)。随包 celld 仍为 v0.2.0。
