# v0.4.2 范围(本刀实现)

> 版本号 **`0.4.2`**。补上 v0.4.1 留下的出路 + 一条实测存在的崩溃丢数据路径。
> **本文件是实现合同**。默认仍是 BOS。**不宣称**已支持 R2/S3。
> 不移动 v0.3.x / v0.4.0 / v0.4.1。非 BOS 真桶实测仍在 v0.3.8 合同排队(需凭证)。

## 用户能感知什么

- 旧 `.json` 会话可用 `celagent migrate <id>` 一次性转成 Pi JSONL,之后 `celagent <id>`
  打开的是真 Pi 会话(工具调用 / thinking / 分支都在),不再停在文本注入模式。
- 上轮进程崩在「本地已追加、BOS 未落地」之间时,下次启动**保留本地并补写回 BOS**,
  不再被远端旧快照覆盖掉那几轮。
- BOS 短暂不可读导致「本次不做权威写入」后,恢复可读会**自动升级**继续写,不必重启。
- 单会话 JSONL 超 5 MB 时明确提示「每轮整文件重传」的流量成本。

## 做

### P0 — 旧格式的出路(v0.4.1 只堵不疏)

- `jsonlFromTurns(sessionId, turns)`:旧 turns → Pi JSONL(version 3 头 + parentId 单链)。
  - 可恢复的原样保留:user/assistant 的完整 content 块(text / thinking / toolCall)、
    toolResults 的 toolName+content、时间戳。
  - `toolCallId` 按「第 N 个 toolCall ↔ 第 N 个 toolResult」重链,并用 toolName 交叉校验
    (二者由同一 `turn_end` 事件按序捕获)。
  - **不可恢复的元数据不伪造**:assistant 的 `api/provider/model/usage/stopReason` 与
    toolResult 的 `isError` 旧格式从未采集 —— 统一用自我标识的占位
    (`MIGRATED_MODEL = "unknown-migrated-v03-turns"`,usage 全零,stopReason `stop`),
    并在 JSONL 头部 `migratedFrom` 记明来源,读到的人一眼知道它不是原始采样。
- `celagent migrate <id> [--bucket B] [--yes]`:
  - 目标 `.jsonl` 已存在 → 不动;目标状态读不确定 → 中止(不赌)。
  - 转换后**先写临时文件用 `SessionManager.open` 自检**,载入 message 数与转换数不符
    或 Pi 打不开 → **不写入任何对象**。合成元数据的正确性靠实测,不靠断言。
  - 写入用 `ifNoneMatch`(首写保护);冲突 → 放弃,不覆盖。
  - **旧 `.json` 永不删除**,保留为备份。
  - 非 TTY 需 `--yes`,与 `rm` 一致。

### P1 — 崩溃恢复(修实测存在的丢数据路径)

- 启动 `openedFromJsonl` 分支原先**无条件**用远端覆盖本地文件:若本地因崩溃而领先,
  那几轮被永久毁掉且本可恢复。现在本地是远端的**谱系超集**时(复用 `jsonlSupersedes`)
  保留本地并 `queueJsonlWrite` 补写回;谱系不同源 / 本地落后 / 本地损坏 → 仍以远端为准。

### P2

- `persistMode="blocked"` 自愈:按 30s 冷却重探远端,可读后升级回 `jsonl`/`legacy`,
  并立即补写当前轮,不必重启进程。
- 写放大告警:单会话 JSONL ≥ `JSONL_SIZE_WARN_BYTES`(5 MB)时提示整文件重传成本。

## 明确不进 v0.4.2

- **写放大的真解(分段 `sessions/<id>/seg-N.jsonl` + manifest)**:S3 无追加原语,
  单会话累计流量仍是 O(n²)(500 轮 / 2.4 MB 文件 ≈ 累计 1.2 GB)。本刀只告警不优化。
- **debounce 合并权威写**:会给权威写引入延迟窗口 —— 用 RPO=0 换适度流量是坏交易,
  不做。
- 多端同写的条目级树合并(当前语义:分叉方被拒并警告)。
- 批量迁移 / 自动迁移(必须逐个显式确认)。
- 真 R2/S3 联调(→ v0.3.8 合同,需凭证)。

## 验收

- [x] `jsonlFromTurns` 产出合法 Pi JSONL(首行 type=session、version 3、parentId 单链)
- [x] `toolCallId` 正确重链;thinking / text 块原样保留
- [x] 占位元数据自我标识,不像真实模型名;`migratedFrom` 记明来源
- [x] **迁移产物实测可被 `SessionManager.open` 打开**(回归用例,无 pi 则 skip)
- [x] 迁移产物可继续被谱系保护追加(`jsonlSupersedes` 自洽)
- [x] 写放大告警不阻止写入
- [x] 源码锚定:目标已存在不动、自检不过不写、`ifNoneMatch`、绝不删旧对象、
      崩溃恢复保留本地、blocked 自愈
- [x] `npm test` 全绿;`node --check` / `bash -n` 通过
- [x] 版本号 0.4.2(package.json / TUI / install.sh)

## 发版

实现合进 `main` 后打 tag **`v0.4.2`**(不移动旧 tag)。随包 celld 仍为 v0.2.0。
