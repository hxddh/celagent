# celagent 分布式 agent 运行时 — 多机部署

> 目标: agent 会话/任务状态在 celld cell 中持久, 跨节点/跨机器可迁移;
> BOS 为权威数据层 (sessions/), celld cells 为执行状态层。

## 架构 (分层并存)

```
┌────────────────────────────────────────────────────┐
│ 客户端: celagent CLI (任意机器)                     │
│   - 对话 / 工具 (本地文件系统)                      │
│   - 恢复: worker 缓存快路径 → BOS 权威              │
└──────────────┬─────────────────────────────────────┘
               │ HTTP (任一节点)
┌──────────────▼─────────────────────────────────────┐
│ 执行层: celld 集群 (多机, 经 BOS 发现彼此)          │
│   - 节点注册: BOS nodes/ (addr/负载/所有权代数)      │
│   - 会话即 cell: session:<id> 状态 (休眠/唤醒/迁移)  │
│   - 任务状态机: submit/status/ledger (exactly-once) │
│   - 跨 cell 委托: delegate (agent A → B)            │
└──────────────┬─────────────────────────────────────┘
               │ 权威落盘
┌──────────────▼─────────────────────────────────────┐
│ 数据层: BOS 对象存储 (唯一 bucket)                  │
│   - sessions/<id>.json   会话权威 (CAS 保护)        │
│   - cells/*/ltx          执行状态 (LTX 日志)        │
│   - nodes/ fleet/ wake/  集群协调元数据              │
│   - deploy/              worker 代码分发            │
└────────────────────────────────────────────────────┘
```

## 多机部署步骤

### 1. 每台机器安装

```bash
# celagent + celld + 相同配置 (同一 bucket!)
CELAGENT_SRC=~/celagent ./install.sh
# 或: curl -fsSL <发布地址>/install.sh | sh

# 确认 settings.json 的 bucket 与集群一致
cat ~/.config/celagent/settings.json
# → persistence.bucket 必须相同 (集群共享状态)
```

### 2. 加入集群

```bash
# 每台机器 (端口可自定义, advertise 填本机可达地址)
./scripts/cluster_mgr.sh add-node 19000 192.168.1.50:19000
# 机器 B: add-node 19001 192.168.1.51:19001
# 机器 C: add-node 19002 192.168.1.52:19002
```

节点经 BOS `nodes/` 注册表自动发现彼此,无需手动配置 peer。

### 3. 验证集群

```bash
./scripts/cluster_mgr.sh status
# → BOS 注册表显示所有节点

# 跨节点会话: 机器 A 写入, 机器 B 恢复
celagent my-session          # 在 A 上对话
celagent my-session          # 在 B 上继续 (worker 缓存或 BOS 权威)
```

### 4. 故障验证

```bash
# kill 任意节点 → agent 会话不丢:
#  - 对话继续 (BOS 直写不依赖节点)
#  - 恢复继续 (另一节点 cell 状态 / BOS 权威)
pkill -f 'celld.*19000'      # 模拟机器 A 宕机
celagent my-session          # 在 B 上恢复 — 历史完整
```

## 已验证能力 (2026-08-10 实测)

| 能力 | 验证结果 |
|------|---------|
| 多节点注册 (BOS nodes/) | ✓ 节点经 BOS 发现 |
| 跨节点会话访问 | ✓ 18090 写 → 19000 读 2 轮 |
| 休眠/唤醒 (hibernate/wake) | ✓ 2 轮完整恢复 |
| 任务状态机 (submit/status) | ✓ 5/5 步骤完成, ledger exactly-once |
| 会话级单写者 (epoch fencing) | ✓ BOS CAS (ETag+If-Match), 并发 412 拒绝 |
| agent 直连 BOS 产物 (obj-put) | ✓ 经 webhook 代理写入 (worker 零凭证) |
| 跨 cell 委托 (delegate) | ✓ API 就绪 (celagent task 未接, 测试语义) |

## 安全说明

- 所有节点共享同一 BOS 凭证 (~/.aws/credentials [bos]) — 只读/写自己的 bucket
- worker 不持凭证: obj-put 经本地 webhook 代理签名 (BOS_AK/SK 在 webhook 端)
- 节点间无直接信任: fleet/peer-auth.json 做探测签名
