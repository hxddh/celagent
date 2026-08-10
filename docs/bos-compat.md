# BOS × Celld 兼容性边界文档

> 基于多轮实测 (2026-08-07) 整理的百度 BOS 与 Celld 组合的兼容性边界与避坑指南
> 配套测试: `scripts/celld-bos-test.sh` (10/10 通过)

## 一、BOS 对 Celld 核心原语的支持（全部实测确认 ✅）

| Celld 依赖 | BOS 支持 | 实测证据 |
|-----------|---------|---------|
| **If-None-Match: \*** | ✅ | 已存在 → 412 PreconditionFailed |
| **If-Match: ETag** | ✅ | 正确 etag → 成功; 错误 → 412 |
| **强一致 GET/LIST** | ✅ | PUT 后立即可见 |
| **单 key 原子更新** | ✅ | CAS 语义正确 |
| **节点 lease 写入** | ✅ | nodes/*.json 正常 |
| **LTX 复制** | ✅ | cells/*/ltx/*.ltx 落盘 |
| **alarm wake 索引** | ✅ | wake/ 前缀 LIST 正常 |

**结论: BOS 完全满足 Celld 的 coordination primitive 需求**（这是 Celld 能跑在 BOS 上的前提）。

## 二、已知坑（实测踩过）

### 1. aws CLI 不接受 `/dev/null` 作为 body
```
aws s3api put-object --body /dev/null
→ Error parsing parameter '--body': Blob values must be a path to a file
```
**解决**: 用真实临时文件 `/tmp/body.txt`。

### 2. boto3 直连签名失败
```
boto3.client('s3', endpoint_url='https://s3.bj.bcebos.com')
→ SignatureDoesNotMatch
```
**解决**: 用 aws CLI（`AWS_PROFILE=bos` 环境变量）或 celld（Rust object_store）。
**原因**: BOS 的 SigV4 实现与 AWS 有细节差异, boto3 的签名不兼容。

### 3. `--profile bos` 参数 vs `AWS_PROFILE=bos` 环境变量
```
aws s3api ... --profile bos     # 某些操作签名失败
AWS_PROFILE=bos aws s3api ...   # 正常
```
**解决**: 统一用环境变量 `AWS_PROFILE=bos`。

### 4. `list-objects-v2` 的 KeyCount 不存在 + 分页多行
```
--query 'KeyCount'      → None (BOS 不返回该字段)
--query 'length(Contents)'  → 分页时输出多行 (1000, 1000, 580...)
```
**解决**: 用 `length(Contents)` + `--no-paginate` + `| head -1`。

### 5. 双节点同时启动触发 BOS 限流
```
两个 celld 节点同时启动 → operation timed out / InvalidArgument
```
**解决**: 串行启动 + 间隔 (脚本已处理) + 失败重试。

### 6. cell 迁移时 RestoreFailed（重要架构边界）
```
写会话后立即 kill 节点 → 接管节点 RestoreFailed
原因: LTX 复制是异步的, kill 时可能未完成
```
**解决**: 等待 LTX 复制完成 (实测 ~10s) 再 kill。
**注意**: 这是 Celld 的 RPO 边界——output gate 保证 ACK 前已复制, 但 checkpoint API 不等 LTX。

### 7. get-object 取内容
```
--query 'Body' 返回 None
```
**解决**: 下载到文件再 cat。

### 8. 节点强杀后 own.json 残留（RestoreFailed 的重要根因）
```
场景: 节点被 kill → 新节点启动 → 写 cell 持续 RestoreFailed
原因: own.json 的 owner 指向已死节点 (lease 过期但 own.json 未释放)
     新节点 CAS 接管要求 epoch 匹配, 残留的旧 ownership 阻塞接管
```
**解决**: 删除残留 own.json (模拟 dead-node GC) + 重启节点。
**注意**: Celld 的 dead-node GC 依赖 wake 扫描定时器, 强杀场景下清理不及时。
**建议**: 运维脚本在节点重启后检查/清理过期 ownership; 或接受 Celld 的 GC 延迟。

## 三、Celld 接 BOS 的架构要点

```
写路径:   SQLite WAL → LTX → S3 PUT (异步批量)
协调:     ownership CAS (If-Match/If-None-Match on own.json)
恢复:     接管节点从 S3 拉 LTX → 重建 SQLite
成员:     nodes/*.json lease (TTL 10s)
定时:     wake/ 前缀 (分钟精度字典序)
```

## 四、RPO 边界（实测确认）

| 场景 | 行为 |
|------|------|
| 正常写 + 等 LTX 复制 | ✅ 节点故障后完整恢复 |
| 写后立即 kill（<10s） | ⚠️ 可能 RestoreFailed（LTX 未复制完）|
| output gate 保护的写 | ✅ ACK 前已复制（Celld 内部保证）|

**实际应用**: 关键写应走 Celld 的 output gate 语义（等 durable 再 ACK），或接受 checkpoint API 的异步复制窗口。

## 五、建议

1. **测试基线**: 每次改动后跑 `celld-bos-test.sh` 确认 10/10
2. **生产配置**: 双节点 + BOS, 串行启动, 监控 RestoreFailed
3. **容量**: BOS 小对象 PUT P99 决定写延迟, 高频场景需关注
