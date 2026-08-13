# celagent

独立开源 agent — **Pi 完整 TUI + Celld/BOS 对象存储持久化 (RPO=0)**。
会话权威落盘在对象存储:CAS 成功后可跨机恢复。恢复读 BOS;worker 仅作 miss 回退。

## 特性

- **完整 Pi TUI**:复用 pi-coding-agent 引擎(不 fork),bash/read/write/grep/find/edit/ls 全量工具,多模型切换
- **会话权威在 BOS (RPO≈0)**:每轮对话双写 — worker 缓存 + **BOS 直写**(CAS 乐观锁 + 幂等去重 + 异步队列);恢复先读 BOS
- **跨机恢复**:`celagent <id>` 从 BOS 恢复完整历史,换机器/本地数据丢失都能找回
- **分布式任务**:`celagent task submit/status/ledger` — celld 状态机,断点续跑 + 单 cell ledger 去重(exactly-once 限于同一 cell; 多机见 docs/distributed-deployment.md)
- **本地会话恢复**:TUI 内 `/resume` 切换本机会话,`/new` 开新会话(自动独立持久化 ID)
- **一键部署**:setup.sh 检测凭证 → 建 bucket → 部署 worker → 启动双节点 → 写配置
- **settings 配置**:`~/.config/celagent/settings.json` 自定义

## 安装

```bash
# 一键安装 (GitHub Release 二进制, 含 celld 运行时)
curl -fsSL https://github.com/hxddh/celagent/releases/latest/download/install.sh | sh

# Windows (仅 CLI; 上游 celld 无 Windows 包)
irm https://github.com/hxddh/celagent/releases/latest/download/install.ps1 | iex

# 或开发模式 (源码目录需含 bin//src//package.json, 软链直指源码改即生效)
CELAGENT_SRC=~/celagent ./install.sh
```

## 使用

```bash
export DEEPSEEK_API_KEY=sk-xxx    # 真实 LLM (deepseek, OpenAI 兼容)

celagent                   # 启动 TUI (自动生成唯一会话 ID)
celagent <id>              # 续写指定会话 (从 BOS 恢复历史)
celagent list              # 列出 BOS 里所有可恢复会话
celagent export <id>       # 导出会话 JSON
celagent rm <id>           # 删除会话 (需确认)
celagent doctor            # 自检: 配置/凭证/节点/BOS 连通
celagent config get persistence.bucket
celagent config set model deepseek-v4-flash
celagent task submit write-report 5   # 提交分布式任务 (celld 状态机, 断点续跑)
celagent task status <taskId>         # 任务状态
celagent task ledger                  # 幂等 ledger (单 cell 去重)
celagent version           # 显示版本
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
                    轮次: {turn, role, msg, ts, content, toolResults}  (完整记忆, 不截断)
```

恢复优先级:**BOS 是权威源** — 启动 `celagent <id>` 先从 BOS 读完整历史,仅 BOS miss 时才回退 worker 缓存。BOS 同时持久化 **user 与 assistant** 轮;`/resume` 从本地 JSONL 恢复;`/new` 与 `/fork` 均分配独立持久化 ID。

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
| `celagent doctor` | 六维自检(含 CAS) |
| `celagent cas-probe` | 探测存储条件写(RPO=0 门禁) |
| `celagent task submit <type> [steps]` | 提交分布式任务 (celld 状态机, 断点续跑) |
| `celagent task status [taskId]` | 任务状态 |
| `celagent task ledger` | 幂等 ledger (单 cell 去重, 非跨节点共识) |
| `/resume` (TUI 内) | 切换本地会话 |
| `/new` (TUI 内) | 新会话,打印持久化 ID |

## 配置

`~/.config/celagent/settings.json`:

```json
{
  "provider": "deepseek",
  "model": "deepseek-v4-flash",
  "persistence": {
    "bucket": "celagent-<rand>-<ts>",
    "endpoint": "https://s3.bj.bcebos.com",
    "region": "bj"
  }
}
```

凭证:环境变量 `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`,或 `~/.aws/credentials` 的 profile(默认 `[bos]`,可用 `persistence.profile` 覆盖;两者不混用)。

非 BOS 合格 endpoint(AWS S3 / Cloudflare R2 / Tigris 的 https host)可以 `config set persistence.endpoint`,并必须显式 `persistence.region`(R2 一般为 `auto`)。这只表示配置能写进去;**不表示该后端已实测 CAS**。非法地址会报错,不会静默写到百度 BOS。

install.sh 可配置环境变量(全部有默认值):

| 变量 | 作用 | 默认 |
|------|------|------|
| `CELAGENT_REPO` | 正式模式源码仓库地址 | `https://github.com/hxddh/celagent.git` |
| `CELAGENT_SRC` | 开发模式本地源码目录 | 空(正式模式走仓库) |
| `CELAGENT_ROOT` | 安装根目录 | `~/.local` |
| `CELAGENT_BUCKET` | 强制指定 bucket(覆盖复用逻辑) | 空(自动创建/复用) |
| `CELLD_ESBUILD` | esbuild 路径(worker 部署用) | 自动探测 |

## 架构

```
celagent (TUI, bin/celagent-tui.mjs)
   ├── pi-coding-agent 引擎 (交互/LLM/工具)
   ├── Celld 节点 (18090/18091 自动启动, 19000 候选, 端口预检)
   └── BOS 直写 (src/bos.js, CAS + 重试 + 异步队列)
```

## 开发

```bash
cd <仓库路径>
node --version                         # 需 >= 22 (pi 依赖链 undici 8.x 要求)
npm install                            # 依赖 (pi 引擎等)
CELAGENT_SRC=<仓库路径> ./setup.sh     # 首次: 建 bucket + 部署 worker + 写配置 + 启动节点
node bin/celagent-tui.mjs doctor       # 自检
npm test                               # 回归 (core + proof; Celld 用例需: scripts/node_mgr.sh start)
./scripts/node_mgr.sh start|stop|status|restart
```

## 测试记录

- 回归: `npm test` (core CLI + proof 源码锚定; Celld/BOS 用例无节点时 skip)
- BOS 链路: 写→读→ETag→CAS 冲突→并发写 (无重复无丢失)
- 20 次高频写压测: 序号连续 1-20 无重复
- Bug 1-97 修复: 覆盖持久化完整性、并发安全、信号处理、安装部署、worker 部署、凭证混用、首轮丢失

## 开发者 / Agent 接手

- **HANDOFF.md** — 项目交接文档(代码地图/发布状态/工程约定),新 agent 或开发者从它开始
- **docs/architecture.md** — 架构权威(三层模型/数据流/机制原理/设计决策/扩展点)
- **PACKAGING.md** — 打包与发布流程
- **docs/distributed-deployment.md** — 多机部署

## License

MIT
