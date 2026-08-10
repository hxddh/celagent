# P0 验证结果: pi-agent-core 可库化 + 状态可接 Celld

日期: 2026-08-07 | 环境: 独立目录 ~/celagent (未碰 ~/.pi)

## 验证结论 (全部通过)

1. **pi-agent-core 可库化** ✅
   - `Agent` 类可直接 import 驱动 (npm 包 v0.84.1)
   - streamFn 注入式设计, 真实场景用 pi-ai 的 `streamSimple`
   - subscribe 事件流可用 (12 事件)

2. **SessionStorage 可替换** ✅
   - 完整抽象接口: getMetadata/getLanes/appendEntry/appendRecord/findEntries 等 ~15 方法
   - 可自定义实现映射到 Celld (checkpoint/resume API)
   - 这是"状态接 Celld"的官方扩展点

3. **checkpoint → Celld 链路** ✅
   - 自定义 storage 写入 → Celld resume 读回 2 轮确认
   - RPO=0 持久化复用已验证方案

## 产品底座结论

独立 CLI 产品 (celagent) 技术底座成立:
- Agent loop: pi-agent-core (库)
- 状态持久: 自定义 SessionStorage → Celld (RPO=0)
- LLM: pi-ai (多 provider)
- 交互: 自写 CLI 壳

## 下一步

1. 产品骨架: CLI 壳 + CelldSessionStorage 完整实现
2. 一键安装脚本 (celld 内置 + 配置引导)
3. settings.json persistence 段
