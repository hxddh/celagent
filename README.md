# celagent

独立开源 agent — **Pi 完整 TUI + Celld/BOS 对象存储持久化 (RPO=0)**。
会话经对象存储权威落盘:崩溃、换机器、节点故障,历史一条不丢。

## 特性

- **完整 Pi TUI**:复用 pi-coding-agent 引擎(不 fork),bash/read/write/grep/find/edit/ls 全量工具,多模型切换
- **会话永不丢 (RPO=0)**:每轮对话双写 — worker 缓存 + **BOS 直写**(CAS 乐观锁 + 幂等去重 + 异步队列),BOS 是权威源
- **跨机恢复**:`celagent <id>` 从 BOS 恢复完整历史,换机器/本地数据丢失都能找回
- **本地会话恢复**:TUI 内 `/resume` 切换本机会话,`/new` 开新会话(自动独立持久化 ID)
- **一键部署**:setup.sh 检测凭证 → 建 bucket → 部署 worker → 启动双节点 → 写配置
- **settings 配置**:`~/.config/celagent/settings.json` 自定义

## 安装

```bash
# 一键安装 (GitHub Release 二进制, 含 celld 运行时)
curl -fsSL https://github.com/hxddh/celagent/releases/latest/download/install.sh | sh

# 或开发模式 (源码)
CELAGENT_SRC=~/celagent ./install.sh
```

## 使用

```bash
export DEEPSEEK_API_KEY=sk-xxx    # 真实 LLM (deepseek, OpenAI 兼容)

celagent                   # 启动 TUI (自动生成唯一会话 ID)
celagent <id>              # 续写指定会话 (从 BOS 恢复历史)
celagent list              # 列出 BOS 里所有可恢复会话
celagent export <id>       # 导出会话 JSON
celagent doctor            # 自检: 配置/凭证/节点/BOS 连通
celagent config get persistence.bucket
celagent config set model deepseek-v4-flash
celagent help              # 全部命令
```

无 `DEEPSEEK_API_KEY` 时自动降级为 mock 回复(验证链路用)。

## 会话持久化模型

```
TUI 交互 (pi-coding-agent 引擎, 全量工具)
   │  turn_end 钩子 (不阻塞对话)
   ├─▶ worker SQLite (快速缓存, 2s 超时, 丢了可重建)
   └─▶ BOS 直写队列 (权威源, CAS If-Match 乐观锁 + 幂等去重)
                                │
                    sessions/<id>.json
                    轮次: {turn, role, msg, ts}
```

恢复优先级:**BOS 是权威源** — 启动 `celagent <id>` 时从 BOS 读历史注入上下文;`/resume` 从本地 JSONL 恢复完整会话。

## agent 内置 BOS 记忆工具 (P1)

对话中 agent 可主动调用(经 customTools 注入):

| 工具 | 功能 |
|------|------|
| `history_search` | 跨会话检索 BOS 历史记忆(关键词/限定会话/条数), 返回匹配轮次片段 |
| `session_snapshot` | 将当前会话状态保存为 BOS 快照(显式记忆锚点, snapshots/ 前缀) |

示例: agent 可说 "搜索一下我之前关于并发问题的讨论" → 调用 history_search 找回并引用。

## 命令一览

| 命令 | 作用 |
|------|------|
| `celagent` | 启动 TUI,唯一会话 ID |
| `celagent <id>` | 续写 BOS 会话 |
| `celagent list [--bucket B]` | 列会话(settings 丢失自动扫描 bucket) |
| `celagent export <id>` | 导出 JSON |
| `celagent rm <id>` | 删除会话(需确认) |
| `celagent config get/set` | 配置读写 |
| `celagent doctor` | 四维自检 |
| `/resume` (TUI 内) | 切换本地会话 |
| `/new` (TUI 内) | 新会话,打印持久化 ID |

## 配置

`~/.config/celagent/settings.json`:

```json
{
  "provider": "deepseek",
  "model": "deepseek-v4-flash",
  "persistence": {
    "bucket": "celagent-<user>-<ts>",
    "endpoint": "https://s3.bj.bcebos.com",
    "region": "bj"
  }
}
```

凭证:环境变量 `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`,或 `~/.aws/credentials` 的 `[bos]` profile(自动读取)。

## 架构

```
celagent (TUI, bin/celagent-tui.mjs)
   ├── pi-coding-agent 引擎 (交互/LLM/工具)
   ├── Celld 双节点 (18090/18091, 自动启动 + 端口预检)
   └── BOS 直写 (src/bos.js, CAS + 重试 + 异步队列)
```

## 开发

```bash
cd ~/celagent
node bin/celagent-tui.mjs doctor      # 自检
node tests/core.test.mjs              # 回归测试 (需节点在跑: scripts/node_mgr.sh start)
./scripts/node_mgr.sh start|stop|status|restart
```

## 测试记录

- 回归: core.test.mjs 5/5 (Celld API / checkpoint+resume / kv / Agent 构造 / 工具)
- BOS 链路: 写→读→ETag→CAS 冲突→并发写 (无重复无丢失)
- 20 次高频写压测: 序号连续 1-20 无重复
- Bug 1-65 修复: 覆盖持久化完整性、并发安全、信号处理、安装部署

## 开发者 / Agent 接手

- **HANDOFF.md** — 项目交接文档(架构/代码地图/发布状态/工程约定),新 agent 或开发者从它开始
- **PACKAGING.md** — 打包与发布流程
- **docs/distributed-deployment.md** — 多机部署

## License

MIT
