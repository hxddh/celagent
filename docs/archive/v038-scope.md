# v0.3.8 范围(下一刀,需凭证)

> 版本号 **`0.3.8`**。v0.4.0 发版之后的下一刀:**至少一种非 BOS 合格后端的真桶实测**。
> **本文件是实现合同**。默认仍是 BOS。无 R2/S3 凭证 **不开实现 PR、不打 tag、不宣称已支持**。
> 不移动 v0.3.0–v0.4.0。CI 内存探针 / MinIO / LocalStack **不算**实测。
> v0.4.0 插入了 Pi JSONL 权威会话,本刀范围不变。

## 用户能感知什么

- `persistence.endpoint` 指向 Cloudflare R2(优先)或 AWS S3 时,`celagent doctor` / `cas-probe` 在真桶上通过 If-None-Match / If-Match / 写后读。
- 文档把该后端从「未测」改成「已实测」。BOS 默认路径行为不变。
- 不要把 v0.3.5 的 region 贯穿或 v0.3.3 的白名单写成「已支持 R2」。

## 做

### P0 — 必须真桶

- 预建测试桶 + API 凭证(R2 `region=auto` 或 S3 对应 region)。
- `cas-probe` 对该后端 exit 0;transient(网络)与结论性失败仍按 v0.3.6 语义。
- 一轮 `persistTurnToBos` 后,另一进程 `loadSessionHistory` 读回 CAS 成功的对象。
- `doctor` 打印实际 endpoint / region / profile。
- `docs/s3-compat-evaluation.md` / README / HANDOFF:仅把真正跑过的后端标「已实测」。

### P1

- setup/install 对非 BOS 继续「桶必须已存在」(v0.3.3),不在陌生账号里建桶。

## 明确不进 v0.3.8

- 无凭证的「已支持」矩阵
- MinIO / LocalStack / 内存 store 冒充非 BOS 实测
- GCS / Azure / rename `bos.js`
- provider 认证 / 快照 TUI / 会话合并
- TUI 自动清理共享桶 `own.json`、CELD_NODES 发现远程节点

## 验收

- [ ] 真桶 `cas-probe` 通过
- [ ] 真桶写一轮、另一进程读回
- [ ] 文档「已实测」只列真正跑过的后端
- [ ] BOS 回归不退步(`npm test` + 有凭证时 BOS 用例)
- [ ] 无凭证环境不得合入「已支持 R2」文案

## 发版

有真桶验收后再打 tag **`v0.3.8`**(不移动旧 tag)。随包 celld 仍为 v0.2.0,除非 celld 新版本有实测必要。
