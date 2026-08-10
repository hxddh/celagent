// POC: 用 Pi 的 createAgentSession 驱动会话 (只读引用 ~/.pi, 不修改)
// 验证: ① createAgentSession 可用 ② 独立 agentDir ③ turn_end 钩子
import { createRequire } from "node:module";

// 从 ~/.pi 引用 pi-coding-agent (只读, 不修改)
const require = createRequire(import.meta.url);
const piPath = "~/.pi/agent/node_modules/@earendil-works/pi-coding-agent";
const pi = await import(`file://${piPath}/dist/index.js`);

console.log("=== POC: 用 Pi 的 createAgentSession ===");
console.log("pi-coding-agent 导出:", Object.keys(pi).filter(k => k.includes("Session") || k === "createAgentSession").slice(0, 8));

// 检查 createAgentSession 是否存在
if (!pi.createAgentSession) {
  console.log("❌ createAgentSession 不可用, 导出有:", Object.keys(pi).slice(0, 15));
  process.exit(1);
}
console.log("✅ createAgentSession 可用");

// 检查依赖的 Agent/SessionManager
console.log("依赖检查:");
for (const name of ["Agent", "SessionManager", "SettingsManager", "ModelRegistry", "AgentSession"]) {
  console.log(`  ${name}:`, pi[name] ? "✓" : "✗");
}

// 检查事件钩子 (turn_end 等)
console.log("ExtensionRuntime:", pi.createExtensionRuntime ? "✓" : "✗");
