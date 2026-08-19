# v0.4.0 范围(本刀实现)

> 版本号 **`0.4.0`**。对 v0.3.7 之后的**质变**:对象存储上的权威会话从「轮次 JSON + steer 作文」换成 **Pi 原生 JSONL**,`celagent <id>` 用 `SessionManager.open` 打开真会话。
> **本文件是实现合同**。默认仍是 BOS。**不宣称**已支持 R2/S3。
> 原定 v0.3.8 的非 BOS 真桶实测**仍顺延**(需凭证)。不移动 v0.3.0–v0.3.7。

## 用户能感知什么

- `celagent <id>` 打开的是 BOS 上的 Pi 会话:工具调用、thinking、树/分支、compaction 都在,不再是「把最近 50 轮文本贴进一条 steer」。
- 本地 JSONL 文件名 stem = persistId = BOS key `sessions/<id>.jsonl`。下次仍用同一个 `celagent <id>`。
- 旧 `sessions/<id>.json` **还能读**:走原来的文本注入,并标明「旧格式」。新写入只写 JSONL。
- `list` / `export` / `rm` / `history_search` 认 JSONL;同一 id 同时有新旧对象时以 JSONL 为准。

## 做

### P0

- 权威对象:`sessions/<id>.jsonl`(Pi JSONL,首行 `type:"session"`)。CAS If-Match / If-None-Match、GET/PUT 瞬时失败留队、403 不重试、损坏不覆盖 — 与 v0.3.7 同一套门禁。
- 恢复:`loadSessionHistory` **JSONL 优先**;仅 JSONL miss 才读旧 turns JSON;再 miss 才 worker。JSONL 损坏不回退旧 JSON。
- TUI:`SessionManager.open(localJsonl)` 载入;JSONL 路径**禁止** steer。打开失败 fail-closed(不覆盖 BOS)。
- `persistId` = JSONL 文件名 stem(`persistIdFromJsonlPath`),`/new` `/fork` `/resume` 都绑到 `sessions/<id>.jsonl`。
- 队列:`queueJsonlWrite` 写整份 JSONL;同会话未执行任务合并为最新快照(避免 50 份全量拷贝)。
- 旧 `persistTurnToBos` / `sessions/<id>.json` 保留只读兼容与测试,TUI 主路径不再 `queueBosWrite`。

### P1

- `history_search` 先读 `.jsonl`(JSONL 成功则不再读同 id 的 `.json`)。
- `export` 优先 stdout JSONL;`rm` 删 `.jsonl` 与 `.json`;`list` 去重,JSONL 优先。
- README / architecture / HANDOFF 保证句改为:CAS 成功的 `sessions/<id>.jsonl` 可跨机被 Pi 打开。

## 明确不进 v0.4.0

- 真 R2/S3 联调与「已支持」矩阵(→ v0.3.8,需凭证)
- 把旧 `.json` 批量迁移成 JSONL(读兼容即可,写新会话自然切走)
- TUI 自动清理共享桶 `own.json`、CELD_NODES 发现远程节点
- provider 认证 / 快照 TUI / 会话合并 / rename `bos.js`

## 验收

- [x] `persistJsonlToBos` 首写成功;GET 超时 → `"retry"`;损坏不覆盖;AccessDenied 不重试
- [x] `loadSessionHistory` JSONL 优先;JSONL 损坏不回退 JSON/worker;JSON miss 才读旧 JSON
- [x] 同会话 JSONL 队列合并为最新
- [x] TUI 源码:`SessionManager.open` + `queueJsonlWrite`;JSONL 路径无 `queueBosWrite` / 无 steer
- [x] `npm test` 全绿;`bash -n` 通过
- [x] 版本号 0.4.0(package.json / TUI / install.sh)

## 发版

实现合进 `main`(PR #19,`f9e6e0a`)后打 tag **`v0.4.0`**(不移动旧 tag)。随包 celld 仍为 v0.2.0。
冒烟:`./scripts/release-smoke.sh v0.4.0` → `celagent v0.4.0` + SHA256 通过。
