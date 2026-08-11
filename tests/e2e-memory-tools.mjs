// 真实实测 v2: 打印所有事件 + 等待 LLM 完整响应
import { homedir } from "node:os";
import { join } from "node:path";
import { tmpdir } from "node:os";

const AGENT_DIR = join(homedir(), ".config", "celagent", "pi-runtime");
const pi = await import("@earendil-works/pi-coding-agent");
const { history_search, session_snapshot } = await import("../src/bos-tools.js");

const cwd = homedir();
const settingsManager = pi.SettingsManager.create(cwd, AGENT_DIR, { projectTrusted: true });
const services = await pi.createAgentSessionServices({
  cwd, agentDir: AGENT_DIR, settingsManager,
  modelRuntimeSignal: AbortSignal.timeout(15000),
});
const sessionDir = join(tmpdir(), "celagent-e2e-" + Date.now());
const sessionManager = pi.SessionManager.create(cwd, sessionDir);

const result = await pi.createAgentSessionFromServices({
  cwd, agentDir: AGENT_DIR, services, sessionManager,
  sessionStartEvent: { type: "session_start", reason: "startup" },
  customTools: [history_search, session_snapshot],
});
const session = result.session;

// 模拟 celagent-tui 的快照缓存维护 (真实 TUI 中在 turn_end 钩子里 push)
let snapshotTurns = [];
globalThis.__celagentSnapshotTurns = () => snapshotTurns;

// 打印所有事件类型
const eventCounts = {};
session.subscribe((event) => {
  eventCounts[event?.type] = (eventCounts[event?.type] || 0) + 1;
  if (event?.type === "turn_end") {
    const text = (event.message?.content || []).filter(b => b.type === "text").map(b => b.text).join("");
    snapshotTurns.push({ turn: snapshotTurns.length + 1, role: "assistant", msg: text || "(无文本)", ts: Date.now() });
  }
  if (event?.type === "agent_message") {
    const text = (event.message?.content || []).filter(b => b.type === "text").map(b => b.text).join("");
    if (text.trim()) console.log(`\n[agent 文本] ${text.slice(0, 250)}`);
  }
  if (event?.type === "tool_execution_start") {
    console.log(`\n🔧 [工具调用] ${event.toolName || event.name}(${JSON.stringify(event.arguments || event.params)?.slice(0, 100)})`);
  }
  if (event?.type === "tool_execution_end") {
    const text = typeof event.result === "string" ? event.result : JSON.stringify(event.result)?.slice(0, 180);
    console.log(`📦 [工具结果] ${text}`);
  }
});

console.log(">>> 用户: 搜索'持久化'历史并总结\n");
const p1 = session.prompt("请调用 history_search 工具搜索关键词'持久化',然后把找到的结果简要总结。");
// 等待完成
const r1 = await p1.catch(e => ({ error: e.message }));
console.log("\n[prompt1 返回]", JSON.stringify(r1)?.slice(0, 200));
await new Promise(r => setTimeout(r, 5000));

// 先走一轮真实对话 (触发 turn_end → 快照缓存填充)
console.log("\n\n>>> 用户: 先简短回答, 然后保存快照\n");
await session.prompt("先简单说一句'这是一轮测试对话',然后调用 session_snapshot 把当前会话保存为快照,名称叫'记忆增强实测'。");
await new Promise(r => setTimeout(r, 20000));
console.log("\n\n>>> 用户: 保存快照(第二轮)\n");
const p2 = session.prompt("请调用 session_snapshot 工具,把当前会话保存为快照,名称叫'记忆增强实测'。");
const r2 = await p2.catch(e => ({ error: e.message }));
console.log("\n[prompt2 返回]", JSON.stringify(r2)?.slice(0, 200));

console.log("\n=== 事件统计 ===");
console.log(eventCounts);
process.exit(0);
