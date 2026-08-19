# v0.4.1 范围(本刀实现)

> 版本号 **`0.4.1`**。对 v0.3.7+v0.4.0 变更集深度代码审查挖出的 7 项修复(PR #21)。
> **本文件是实现合同**。核心:v0.4.0 把「按轮 CAS 合并写」换成「整文件替换写」时
> 丢掉了不丢历史的性质,本刀补回 —— 方向(Pi JSONL 权威)不变,写路径加单调性约束。
> 不移动 v0.3.x / v0.4.0。非 BOS 真桶实测仍在 v0.3.8 合同排队。

## 用户能感知什么

- 双机同 id / 忘关的旧 TUI / 启动读抖动后的新会话,**不再能整体覆盖 BOS 上的历史**——
  被拒的一方看到警告,远端数据原样保留。
- 显式 `celagent <id>` 启动时 BOS 读不到(网络/凭证问题)会**拒绝启动**并说明原因,
  不再静默给同一 id 开新会话。
- 几百轮的旧 `.json` 会话打开后,全量历史仍可被加载与 `history_search` 搜到
  (不再被 50 轮摘要的 `.jsonl` 遮蔽)。
- `celagent rm` 在旧格式对象删除失败时报错(不再假报成功后会话复活)。
- settings.json 损坏时给一次明确警告,不再每 1–64s 刷一行重试警告。

## 做

### P0(RPO=0)

- `persistJsonlToBos` 谱系覆盖保护:Pi 会话文件是**追加式日志**(compaction 也是追加条目,
  entry 有稳定 id)——远端条目 id 序列必须是本地前缀才允许整体覆盖(`jsonlSupersedes`)。
  新会话撞 id、别处已写更多、本地落后,一律拒绝且不重试。
- 显式会话 ID 启动遇 BOS 读取 transient/fatal → 拒绝启动(自动生成 id 的新会话不受影响)。
- 旧 `.json` 会话**不隐式迁移** JSONL:继续按轮 CAS 合并写(`persistMode="legacy"`);
  `/resume` 按远端对象类型路由;远端状态未知 → `persistMode="blocked"`,本次拒绝权威写。
  JSONL 迁移留给未来显式命令。

### P1/P2

- `rm`:`.json` 删除失败(非 404)必须报错,提示残留对象会复活会话。
- `history_search`:`.jsonl` 读失败不标 `haveJsonl`(不跳过 `.json` 回退);
  跨会话搜索也上报部分读取失败。
- `resolveStore`:settings.json 的 SyntaxError 判终态 skip,与读路径(当作 no-config)一致。
- `createPersister` 默认 warn 按 channel 去重(恢复 warnOnce 语义)。

## 明确不进 v0.4.1

- 多端同写同一会话的条目级树合并(当前语义:分叉方被拒并警告)
- JSONL 写放大优化(分段/按大小降频)——长会话流量 O(n²),文本会话规模下可接受
- 旧 `.json` → JSONL 的显式迁移命令
- 真 R2/S3 联调(→ v0.3.8 合同)

## 验收

- [x] `jsonlSupersedes`:追加扩展/等同放行;落后/分叉拒绝
- [x] `persistJsonlToBos` 三种拒绝覆盖场景 + 同谱系追加放行(内存 store 可执行回归)
- [x] SyntaxError 终态不重试;默认 warn 去重
- [x] 源码锚定:legacy 按轮合并、启动拒绝、rm 残留报错、search 不跳过回退
- [x] `npm test` 全绿;`bash -n` 通过
- [x] 版本号 0.4.1(package.json / TUI / install.sh)

## 发版

实现合进 `main`(PR #21)后打 tag **`v0.4.1`**(不移动旧 tag)。随包 celld 仍为 v0.2.0。
