# HANDOFF — 项目交接文档(给下一个开发者/agent)

> 本文档是**唯一权威交接入口**。任何 agent 或开发者接手本项目,先从本文档开始。
> 配套文档:`README.md`(用户视角)、**`docs/architecture.md`(架构权威: 数据流/机制/设计决策/扩展点)**、
> `PACKAGING.md`(打包/发布)、`docs/distributed-deployment.md`(多机部署)、
> `docs/s3-compat-evaluation.md`(多后端对象存储评估)、`docs/post-v032-evaluation.md`(v0.3.2 后排期)。

## 0. 项目定位

**celagent**:独立开源 agent 产品。基于 Pi TUI 引擎 + Celld 分布式运行时 + 对象存储,核心卖点是**会话永不丢(RPO=0)**——每轮对话经对象存储权威落盘,崩溃/换机器/节点故障历史一条不丢。默认后端是百度 BOS(唯一实测);扩到其它 S3 兼容存储的条件与计划见 `docs/s3-compat-evaluation.md`。

**核心心智模型**:对象存储保数据(权威源,RPO=0)+ Celld 保执行(缓存/任务/集群)+ agent 可用同一存储(记忆工具)。

```
TUI 交互 (pi-coding-agent 引擎, 全量工具)
   │  turn_end 钩子 (不阻塞对话)
   ├─▶ worker SQLite (快速缓存, 2s 超时, 丢了可重建)
   └─▶ BOS 直写队列 (权威源, CAS If-Match 乐观锁 + 幂等去重)
                                │
                    sessions/<id>.json  (完整 content, 不截断; 默认 BOS)
```

**架构细节(数据流时序/机制原理/组件边界/10 项设计决策/扩展点)见 `docs/architecture.md`**——
改造架构或基于迭代前必读。

## 1. 代码地图

| 路径 | 职责 |
|------|------|
| `bin/celagent-tui.mjs` | CLI + TUI 主程序(~880 行:命令解析含 task 分布式任务、节点自动启动、turn_end 持久化钩子、会话恢复) |
| `src/bos.js` | 对象存储直写核心(aws CLI、CAS If-Match/If-None-Match;默认 BOS,见 s3-compat-evaluation) |
| `src/bos-tools.js` | agent 内置记忆工具:`history_search`(跨会话检索)+ `session_snapshot`(显式快照),经 customTools 注入 pi 引擎 |
| `worker/src/index.js` | Celld worker(缓存读路径、Sync API、产物走 webhook) |
| `worker/wrangler.jsonc` | Celld worker 绑定清单(部署走 `celld deploy`, 不是 Cloudflare wrangler) |
| `install.sh` | 一键安装(正式模式:GitHub Release 下载平台二进制 + celld/worker;开发模式:CELAGENT_SRC) |
| `setup.sh` | 一键部署(凭证检测→建 bucket→部署 worker→启动双节点→写配置) |
| `scripts/node_mgr.sh` | 本机双节点管理(start/stop/status/restart,18090/18091) |
| `scripts/cluster_mgr.sh` | 多机集群管理(add-node/status 等) |
| `scripts/celld-bos-test.sh` | 对象存储链路测试(配置来自 settings;`celld-store-test.sh` 为同义入口) |
| `docs/celld-v02-evaluation.md` | celld v0.2.0 对照评估 + 新特性利用评审(P0 双监听已落地) |
| `docs/s3-compat-evaluation.md` | 多后端对象存储评估:合格/不合格分界、耦合清单、分阶段计划(未改代码) |
| `docs/post-v032-evaluation.md` | v0.3.2 之后排期:候选方向取舍、死代码/fail-open/CI 边界 |
| `docs/v033-scope.md` | v0.3.3 实现合同(已发布: fail-closed + 配置单一来源) |
| `docs/v034-scope.md` | v0.3.4 实现合同(已发布: CAS doctor + 删 SigV4 死代码) |
| `scripts/store_env.sh` | 运维脚本共用的 endpoint/region/profile 读取 |
| `scripts/release-smoke.sh` | 无凭证发布冒烟(下载+SHA256+version/help) |
| `docs/evaluation-followup.md` | PR#2 评估项对照(已在 v0.3.1 落地) |
| `tests/core.test.mjs` | 核心回归(CLI + 可选 Celld/BOS; 无节点时 skip) |
| `tests/review-logic-proofs.test.mjs` | P0 正确性源码锚定(BOS-first/队列丢最旧/fork/parse/steer/seq) |
| `tests/e2e-memory-tools.mjs` | 真实 LLM e2e(需 DEEPSEEK_API_KEY env) |
| `docs/celld-bos-architecture-demo.html` | 架构演示页(单文件、零依赖、33 轮真实对话实录回放, 2026-08-11) |
| `.github/workflows/ci.yml` | CI:syntax check + CLI smoke + 单元测试 + npm pack dry-run |

## 2. 开发环境与命令

### 快速上手(新环境,按序执行)

```bash
# 0. 前置: node >= 22 (pi 依赖链 undici 8.x 不支持 node 20)
node --version
# 1. 依赖 (pi 引擎等)
npm install
# 2. Celld 运行时 (测试/节点必需; 不在仓库内, 需单独安装)
curl -fsSL https://celld.dev/install.sh | sh    # 装到 ~/.local/bin/celld
# 3. BOS 凭证 (测试 BOS 链路必需)
aws configure --profile bos          # 配 AK/SK/region=bj
# 4. (仅首次) 一键部署: 建 bucket + 部署 worker + 写 settings.json + 启动双节点
#    ⚠️ CELAGENT_SRC 必须指向你的仓库路径 (setup.sh 默认找 ~/celagent)
CELAGENT_SRC=<仓库路径> ./setup.sh
# 5. 跑测试 (需节点在跑; 无节点时 Celld/BOS 用例 skip, CLI + proof 仍跑)
npm test
```

### 常用命令

```bash
# 测试 (需节点: 先 scripts/node_mgr.sh start)
npm test                          # core + P0 proof; 无节点时 Celld/BOS skip

# 节点管理
./scripts/node_mgr.sh start|stop|status|restart

# 自检
node bin/celagent-tui.mjs doctor

# 真实 LLM e2e (需要 DEEPSEEK_API_KEY 环境变量)
node tests/e2e-memory-tools.mjs

# 分布式任务 (celld 状态机, 断点续跑)
node bin/celagent-tui.mjs task submit write-report 5
node bin/celagent-tui.mjs task status
node bin/celagent-tui.mjs task ledger
```

### 关键环境事实

- **BOS bucket**:开发用 bucket 名**不在本仓库写死**(安全红线)。查询方式:
  - `cat ~/.config/celagent/settings.json` → `persistence.bucket`
  - 或 `AWS_PROFILE=bos aws s3api list-buckets`(避坑指南: 统一 env 形式)
  - endpoint `https://s3.bj.bcebos.com`,凭证在 `~/.aws/credentials` 的 `[bos]` profile(动态读取)
- **LLM**:DeepSeek(OpenAI 兼容),key 在 `DEEPSEEK_API_KEY` 环境变量(无值时自动降级 mock)
- **凭证优先级**:完整环境变量 `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` 优先(必须成对);否则用 `AWS_PROFILE=bos`(不把 SK 读进脚本变量/Node 堆).两者不混用
- **install.sh 可配置 env**:`CELAGENT_REPO`(仓库地址)、`CELAGENT_SRC`(开发模式)、`CELAGENT_ROOT`(安装根目录,默认 ~/.local)、`CELAGENT_BUCKET`(强制 bucket)、`CELLD_ESBUILD`(esbuild 路径)
- **默认 bucket**:`celagent-<rand8>-<ts>`(随机后缀, 不含 whoami)
- **Celld**:不在仓库内;发布随包 **v0.2.0**(linux-x64/arm64、darwin-arm64)。启动必须双监听:Worker `--listen 127.0.0.1:18090|18091`,内部 `--internal-listen/--advertise` 为 port+2。评估见 `docs/celld-v02-evaluation.md`
- **Pi 引擎**:npm 包 `@earendil-works/pi-coding-agent` v0.84.x(不 fork,库用)

## 3. 发布状态(v0.3.4 已发布)

仓库:`https://github.com/hxddh/celagent`。Latest:[v0.3.4](https://github.com/hxddh/celagent/releases/tag/v0.3.4)。

- **tag `v0.3.4`** 指向 `eec47c4`(PR #11 快进进 main);Release 资产由此 SHA 构建
- **v0.3.3**(`1514b1b`)仍可用,无 CAS 门禁
- **v0.3.2**(`e5ae737`)无 persistence.endpoint 时行为与 v0.3.3 相同
- **v0.3.1**(`6790e99`) spawn 参数按 celld v0.1,配随包 0.2.0 二进制会拒启
- 旧 tag **`v0.3.0`** 仍指向 `31d12a4`;不要用 `git checkout v0.3.0` 当当前代码

### 已完成
- ✅ 代码功能:核心持久化(BOS 直写 + CAS + 幂等)、双节点、分布式部署、worker 缓存、记忆工具(history_search/session_snapshot)、完整记忆(不截断 content);恢复 **BOS-first**
- ✅ 测试:`npm test`(core + proof 源码锚定);Celld/BOS 用例无节点时 skip;e2e 真实 LLM 需 `DEEPSEEK_API_KEY`
- ✅ **安全净化(2026-08-12)**:当前树 + 可达 git 历史已 `filter-repo`;CI 含 Secret/PII 门禁;零密钥硬编码
  - ⚠️ GitHub 对 **已推送过的旧 SHA** 可能仍短期通过直接 commit URL 提供内容;彻底抹掉需向 GitHub Support 申请 purge
- ✅ 构建:`.github/workflows/release.yml` 匿名路径 bun 交叉编译 + 拉取 `denoland/celld` + `SHA256SUMS`
- ✅ **v0.3.4 资产清单**(形态同 v0.3.3):celagent 五平台; celld-linux-x64 / celld-linux-arm64 / celld-darwin-arm64; install.sh / install.ps1 / worker.tar.gz / SHA256SUMS。**差异是 doctor/setup/persist 的 CAS 门禁**
- ✅ 安装校验:`scripts/release-smoke.sh v0.3.4` 下载 linux 包、核对 SHA256、跑 `version`/`help`(输出 `celagent v0.3.4`;不需要 BOS/celld)

### 已知边界(不是本仓库能补的)
- 上游 `denoland/celld` **没有** Intel Mac (`celld-darwin-x64`) 与 Windows 包;`install.sh` 回退 `celld.dev` / Windows 跳过 celld
- celld v0.2 停机:`node_mgr stop` 走 `POST /shutdown?handoff=preserve` + SIGTERM 等 drain;own.json 全量清理仅崩溃残留或 `CELAGENT_CLEAN_OWN=1`
- 真实 BOS 联调 / 多机故障注入需要本机 `[bos]` 凭证与 celld,CI 不跑
- 全新机器带凭证的 `curl | sh` + 建 bucket + TUI 对话,需有 BOS 的机器上验收

### 发布流程(已完成)

0. ✅ docs/archive 已删除
1. ✅ 仓库已推送
2. ✅ CI 绿 (node 22/24)
3. ✅ tag `v0.3.4` 已推送,资产已上传;publish 路径已设 `GH_REPO`(PR #5)
4. ✅ 跨平台 celagent 由 Release workflow 在 `/tmp/anon-build` 编译
5. ✅ install.sh 正式模式从 GitHub Release 下载,有 `SHA256SUMS` 则校验
6. ✅ 无凭证冒烟脚本:`./scripts/release-smoke.sh v0.3.4` 或 `latest`(SHA256 + version/help)

## 4. 版本与里程碑

| 版本 | 内容 | 状态 |
|------|------|------|
| v0.1.x | CLI 骨架 + BOS 直写 + 双节点 | ✅ |
| v0.2.x | 分布式运行时(worker 缓存/sync、休眠唤醒、agent 任务化、cluster_mgr、多机部署文档) | ✅ |
| P1 记忆增强 | history_search + session_snapshot + 完整记忆(不截断) | ✅ 已并入 |
| v0.3.0 | 首次公开 Release(tag 钉在 `31d12a4`; 资产后来被刷新) | ✅ 历史 |
| v0.3.1 | P0–P5 正确性/安全/发版闭环:BOS-first、user 轮、token、endpoint 白名单、Release 全平台 + SHA256SUMS | ✅ 历史 |
| v0.3.2 | celld v0.2 适配:双监听、`CELLD_VAR_` token、timingSafeEqual、drain/diagnose、驻留/admission 调参 | ✅ 历史 |
| v0.3.3 | 存储 P0:endpoint fail-closed、settings 单一来源、合格 host 白名单 | ✅ 历史 |
| v0.3.4 | CAS doctor、setup/persist 拒绝无条件写存储、删 worker SigV4 死代码 | ✅ 已发布 |

下一刀:**v0.3.5** 至少一种非 BOS 合格后端实测(R2 或 S3,需凭证)。不要插队做 provider 认证/快照 TUI/会话合并。不要把本版 CI 内存探针当成「已支持 R2」。

## 5. 工程约定(接手者必须遵守)

1. **中文交流**(代码注释/文档/commit message 用中文)
2. **先分析方案再动手**,不直接改;改动前说明影响面
3. **数据必须真实**——测试/演示数据来自实测,不得虚构(发布物中的演示数字需与实时 BOS 对齐)
4. **仓库安全红线**(发布前已多轮穷尽检查,任何新提交不得引入):
   - 不得提交任何 API key/凭证/密钥(含 git 历史)
   - 不得提交真实本机用户名、本机绝对路径(`/Users/...`、`/home/...`)、内部项目名(`celld-test`、`celagent-poc`)、真实 bucket/session id
   - **禁止在文档里写真实敏感样例当 denylist**(用抽象模式,如 `<local-user>` / `/Users/<user>/`)
   - 凭证一律运行时动态获取(优先 `AWS_PROFILE=bos`; 不全量把 SK 拷进脚本变量/子进程 env)
   - 默认 bucket 名不得含 `whoami`(用随机后缀)
   - 新增文件默认自检:`rg -n 'sk-[a-zA-Z0-9]{20,}|AKIA|ghp_|ALTAK|/Users/[A-Za-z0-9._-]+/' <文件>`
5. 提交作者统一 `hxddh <hxddh@users.noreply.github.com>`(新环境需自行 `git config user.name/email` 设置,开发机已设)
6. 改完跑测试:`npm test`(core + proof; 节点在跑时 Celld/BOS 用例才执行)+ 相关脚本 `bash -n` 语法检查 + CI secret/PII scan

## 6. 已知技术债/注意点

- `install.sh` 正式模式已走 Release 二进制下载;开发模式用 `CELAGENT_SRC`
- `install.sh` 中 celld: Release 随包优先, 回退 `https://celld.dev/install.sh`
- worker 缓存:checkpoint 走 POST JSON, msg 上限 8000 字符(旧 GET URL 兼容仍在);完整数据在 BOS 权威源;恢复路径 **BOS-first**(仅 miss 才回退 worker)
- P0 正确性(2026-08):队列超限丢最旧、ensureLock finally 释放、`/fork` 独立 persistId、JSON 损坏不覆盖、steer 用 content、seq=`max(turn)`、会话 ID 白名单、CI 扫描排除 `node_modules`
- P1(2026-08):user 轮一并落盘; worker `sync` 按 turn 合并; checkpoint 改 POST body; 本机 worker token(无 token 时 fail-open); ledger 先 pending 再 webhook; `rm --yes`
- P2(2026-08):endpoint 白名单; own.json 仅 `celagent-*` bucket 清理; list `--scan`; config 嵌套保护; history_search 默认当前会话; cwrite 锁 TTL; alarm 取最近唤醒点
- P3(2026-08):跨进程 `ensure.lock`; CI `node --check` 失败即失败; failover 测试 resume 原会话; HANDOFF/架构文档与 Release 资产对齐; `cluster_mgr` 传 worker token
- P4(2026-08):CI Release job 编译全平台 celagent + 拉取 denoland/celld + SHA256SUMS; install 支持 linux-arm64/windows; doctor Celld 离线不再报「全部正常」
- HTML 演示页数字已按 2026-08-11 实时 BOS 对齐;回放为 33 轮真实会话实录(脱敏后),
  后续更新数据时保持与 BOS 一致 + 敏感扫描(见红线)
- CI 单元测试已去掉 `continue-on-error`; Secret/PII 扫描排除 `node_modules`
- 发版由 `.github/workflows/release.yml` 在 tag `v*` 时构建并上传;本地可用 `./scripts/prepare-release-assets.sh` / `./scripts/release-smoke.sh`
- **存储多后端 P0(v0.3.3)**:非法 endpoint fail-closed;脚本读 settings。
- **CAS 门禁(v0.3.4)**:doctor/setup/persist 拒绝忽略 If-Match 的存储。真 R2/S3 联调与「已支持」仍未做。
