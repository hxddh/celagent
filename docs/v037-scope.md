# v0.3.7 范围(本刀实现)

> 版本号 **`0.3.7`**。对 v0.3.6 深度审查后仍超前于实现的 RPO 主路径缺口。
> **本文件是实现合同**。默认仍是 BOS。**不宣称**已支持 R2/S3。
> 原定 v0.3.7 的非 BOS 真桶实测**顺延为 v0.3.8**。不移动 v0.3.0–v0.3.6。
> 已发布:[v0.3.7](https://github.com/hxddh/celagent/releases/tag/v0.3.7)。

## 用户能感知什么

- 对象存储 GET/PUT 网络抖动时,本轮写入**留在队列重试**,不再静默丢轮(与 v0.3.6 探针 retry 对齐)。
- `celagent <id>` 遇到 BOS 超时/5xx/403 **不会**把 worker 的 8000 字截断缓存当成完整历史。
- 热恢复日志区分「BOS」与「worker 缓存(BOS miss)」。
- README 保证句改为可辩护口径(CAS 成功的对象可跨机读回;热恢复是最近 50 轮文本注入)。

## 做

### P0

- 抽出 `src/persist.js`(队列 / persistTurnToBos / loadSessionHistory / classifyStoreError),TUI 只编排。
- GET/PUT 瞬时失败(`ETIMEDOUT` / 5xx 等)返回 `"retry"`;权限类(AccessDenied/401/403)不重试。
- JSON 损坏仍拒绝覆盖(不重试)。
- `loadSessionHistory`:仅 `not-found` / 无 bucket 才 `fallbackResume`;瞬时与结论性读失败不回退。
- 内存 store 可执行测试,不只源码锚定。

### P1 文档

- README / architecture / HANDOFF 保证句与恢复语义对齐。
- 真桶实测顺延 v0.3.8。

## 明确不进 v0.3.7

- 真 R2/S3 联调与「已支持」矩阵(→ v0.3.8)
- TUI 自动清理共享桶 `own.json`、CELD_NODES 发现远程节点
- provider 认证 / 快照 TUI / 会话合并 / rename `bos.js`

## 验收

- [x] `persistTurnToBos` GET 超时 → `"retry"`,不写空会话
- [x] PUT 5xx → `"retry"`;AccessDenied → 不重试
- [x] 队列:GET 失败一次后重试写入成功
- [x] `loadSessionHistory` 超时不走 fallback;not-found 才走
- [x] JSON 损坏不覆盖
- [x] `npm test` 全绿;`bash -n` 通过
- [x] 版本号 0.3.7(package.json / TUI / install.sh)

## 发版

实现合进 `main`(PR #17,`fa872c9`)后打 tag **`v0.3.7`**(不移动旧 tag)。随包 celld 仍为 v0.2.0。
冒烟:`./scripts/release-smoke.sh v0.3.7` → `celagent v0.3.7` + SHA256 通过。
