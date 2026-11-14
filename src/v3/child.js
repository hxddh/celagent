// v3/child.js — 子 agent 的本地状态：state / inbox / result
// 这些文件是父会话与子进程之间的协议，不依赖对象存储；对象存储仍是会话权威。
import {
  existsSync, mkdirSync, readFileSync, writeFileSync, renameSync, readdirSync, unlinkSync, appendFileSync,
} from "node:fs";
import { join, dirname } from "node:path";

export const MAX_TASK_CHARS = 20000;
export const MAX_RESULT_CHARS = 8000;

export function delegationDir(configDir, child) {
  return join(configDir, "delegations", child);
}
export function taskPath(configDir, child) {
  return join(delegationDir(configDir, child), "task.md");
}
export function statePath(configDir, child) {
  return join(delegationDir(configDir, child), "state.json");
}
export function resultPath(configDir, child) {
  return join(delegationDir(configDir, child), "result.json");
}
export function inboxDir(configDir, child) {
  return join(delegationDir(configDir, child), "inbox");
}
export function outputLogPath(configDir, child) {
  return join(delegationDir(configDir, child), "output.log");
}

/** 子 agent 的可读输出：人用 celagent logs <child> 看，不进入父会话上下文。 */
export function appendOutput(configDir, child, text) {
  const body = String(text || "").trim();
  if (!body) return;
  ensureDir(delegationDir(configDir, child));
  appendFileSync(outputLogPath(configDir, child), `[${new Date().toISOString()}] ${body}\n`, { mode: 0o600 });
}

function ensureDir(path) {
  mkdirSync(path, { recursive: true, mode: 0o700 });
}

export function atomicWrite(path, text) {
  ensureDir(dirname(path));
  const tmp = `${path}.tmp-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
  writeFileSync(tmp, text, { encoding: "utf8", mode: 0o600 });
  renameSync(tmp, path);
}

export function readJson(path) {
  try { return JSON.parse(readFileSync(path, "utf8")); } catch (e) { return null; }
}

export function readChildState(configDir, child) {
  return readJson(statePath(configDir, child));
}

export function writeChildState(configDir, child, state) {
  ensureDir(delegationDir(configDir, child));
  atomicWrite(statePath(configDir, child), JSON.stringify(state, null, 2) + "\n");
  return state;
}

export function patchChildState(configDir, child, patch) {
  const cur = readChildState(configDir, child) || { child, status: "running" };
  return writeChildState(configDir, child, {
    ...cur,
    ...patch,
    child,
    updatedAt: new Date().toISOString(),
  });
}

export function writeTask(configDir, child, task) {
  ensureDir(delegationDir(configDir, child));
  atomicWrite(taskPath(configDir, child), String(task).replace(/\s+$/u, "") + "\n");
}

export function readTask(configDir, child) {
  try { return readFileSync(taskPath(configDir, child), "utf8").trim(); } catch (e) { return ""; }
}

export function writeChildResult(configDir, child, result) {
  const body = { ...result, child, at: new Date().toISOString() };
  atomicWrite(resultPath(configDir, child), JSON.stringify(body, null, 2) + "\n");
  return body;
}

export function readChildResult(configDir, child) {
  return readJson(resultPath(configDir, child));
}

/** send：写入一条待处理的用户消息；子进程在下一个 turn 边界读取。 */
let inboxSeq = 0;
export function appendInbox(configDir, child, message) {
  const dir = inboxDir(configDir, child);
  ensureDir(dir);
  // 同进程内用单调序号保证 FIFO；跨进程同毫秒才需要随机后缀打破平局。
  const seq = String(inboxSeq++ % 1000).padStart(3, "0");
  const name = `${String(Date.now()).padStart(13, "0")}-${seq}-${Math.random().toString(36).slice(2, 8)}.json`;
  atomicWrite(join(dir, name), JSON.stringify({ message: String(message), at: new Date().toISOString() }) + "\n");
  return name;
}

export function listInbox(configDir, child) {
  const dir = inboxDir(configDir, child);
  if (!existsSync(dir)) return [];
  const out = [];
  for (const name of readdirSync(dir).sort()) {
    if (!name.endsWith(".json")) continue;
    const body = readJson(join(dir, name));
    if (body?.message) out.push({ name, message: body.message, at: body.at });
  }
  return out;
}

export function consumeInbox(configDir, child, name) {
  try { unlinkSync(join(inboxDir(configDir, child), name)); return true; }
  catch (e) { return false; }
}

/**
 * 子进程每一步做什么：
 * - 有 inbox → 先处理追加消息（send）
 * - 没跑过 → 跑 task
 * - 跑完 turn → finalize（写结果）
 * - 已标记 prompted 但 turn 没结束 → interrupted，绝不重放
 */
export function takeNextPrompt({ state, task, inbox = [] }) {
  if (inbox.length > 0) return { kind: "prompt", text: inbox[0].message, source: "inbox", inboxName: inbox[0].name };
  if (!state?.prompted) {
    return task ? { kind: "prompt", text: task, source: "task" } : { kind: "interrupted", reason: "no-task" };
  }
  if (state.promptDone) return { kind: "finalize" };
  return { kind: "interrupted", reason: "turn-unfinished" };
}

export function lastAssistantTextFromTurns(turns) {
  for (let i = (turns?.length || 0) - 1; i >= 0; i--) {
    const turn = turns[i];
    if (turn.role !== "assistant") continue;
    const parts = [];
    if (Array.isArray(turn.content)) {
      for (const block of turn.content) if (block.type === "text" && block.text) parts.push(block.text);
    }
    if (parts.length > 0) return parts.join("\n");
    if (typeof turn.msg === "string" && turn.msg) return turn.msg;
  }
  return "";
}

export function truncateText(text, limit = MAX_RESULT_CHARS) {
  const s = String(text || "");
  if (s.length <= limit) return { text: s, truncated: false };
  return { text: s.slice(0, limit) + "\n… (截断；完整内容见 result.json)", truncated: true };
}
