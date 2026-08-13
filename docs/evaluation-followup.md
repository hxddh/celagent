# 评估后续落地(相对 PR #2)

PR [#2](https://github.com/hxddh/celagent/pull/2) 的深度评估结论已在 **PR #3 / v0.3.1** 落地,本文只记录对照,不重复评估正文。

| 评估项 | 落地 |
|--------|------|
| 恢复 worker-first / 200 字截断 | BOS-first; checkpoint POST, msg≤8000 |
| 仅 assistant 落盘; steer 只用 msg | user+assistant; steer 用 content |
| 队列丢最新; ensureLock 泄漏; `/fork` 串写 | 丢最旧; finally+ensure.lock; fork 独立 persistId |
| JSON parse 失败覆盖; sync 盲写 | parse 失败 return; mergeTurns |
| CI 扫 node_modules; continue-on-error | 已排除; 失败即失败 |
| HANDOFF 卡认证; install 失败当成功 | 已改; create-bucket/deploy 检查退出码 |
| Release 缺 celld-linux / SHA256SUMS | v0.3.1 已含 linux/darwin-arm64 celld + SHA256SUMS |
| worker 无鉴权; endpoint 任意; history 跨会话 | token fail-open; 白名单; 默认当前会话 |
| tag 与资产不一致 | **v0.3.1** 对齐 main |

评估原文仍在 PR #2 分支 `docs/project-evaluation.md`(不合并进 main,避免把已修复问题写成现状)。
