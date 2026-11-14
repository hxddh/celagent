// v3/delegate.js — 把一个任务交给一个完整的 celagent 子 session
// 父会话是唯一的调度者；这里只负责 spawn / wait / send / list，不保存 DAG、不跑调度循环。
import { existsSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import {
  MAX_TASK_CHARS, delegationDir, taskPath, appendInbox,
  patchChildState, readChildResult, readChildState, readTask, writeChildState, writeTask,
} from "./child.js";
import { branchName, createWorktree, removeWorktree, repoDirty, repoHead, repoRoot, worktreePathFor } from "./worktree.js";

const SESSION_ID_RE = /^[A-Za-z0-9._-]{1,128}$/;
// 轮询必须持有事件循环：unref 会让 `celagent delegate wait` 在结果出来前直接退出。
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export function validSessionId(id) {
  return typeof id === "string" && SESSION_ID_RE.test(id) && !id.includes("..");
}

export function newChildId() {
  return `dlg-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
}

export function delegationsRoot(configDir) {
  return join(configDir, "delegations");
}

function listDelegationStates(configDir) {
  const root = delegationsRoot(configDir);
  if (!existsSync(root)) return [];
  const out = [];
  for (const name of readdirSync(root)) {
    if (!validSessionId(name)) continue;
    const p = join(root, name);
    try { if (!statSync(p).isDirectory()) continue; } catch (e) { continue; }
    const state = readChildState(configDir, name);
    if (state) out.push(state);
  }
  return out.sort((a, b) => String(a.createdAt || "").localeCompare(String(b.createdAt || "")));
}

function formatWait(result) {
  const lines = [`status: ${result.status}`];
  if (result.session) lines.push(`session: ${result.session}`);
  if (result.branch) lines.push(`branch: ${result.branch}`);
  if (result.commit) lines.push(`commit: ${result.commit}`);
  if (result.diffStat) {
    const stat = String(result.diffStat).replace(/\n/g, "; ");
    lines.push(`diff: ${stat.length > 400 ? stat.slice(0, 400) + "…" : stat}`);
  }
  if (result.cost || result.tokens) lines.push(`cost: $${Number(result.cost || 0).toFixed(4)} (${Number(result.tokens || 0)} tokens)`);
  if (result.error) lines.push(`error: ${result.error}`);
  if (result.text) lines.push(`text:\n${result.text}`);
  else if (result.status === "running") lines.push("text: (仍在运行)");
  return lines.join("\n");
}

export function createDelegator({
  configDir,
  startSupervisor,
  stopSupervisor = null,
  bin = null,
  node = process.execPath,
  maxChildren = 4,
}) {
  if (!configDir) throw new Error("createDelegator 需要 configDir");
  if (typeof startSupervisor !== "function") throw new Error("createDelegator 需要 startSupervisor");

  async function spawn({ parent, cwd, task, isolate = true }) {
    if (!validSessionId(parent)) throw new Error(`无效父 session: ${parent}`);
    const text = String(task || "").trim();
    if (!text) throw new Error("task 不能为空");
    if (text.length > MAX_TASK_CHARS) throw new Error(`task 太长（${text.length} > ${MAX_TASK_CHARS} 字符）`);
    const running = listDelegationStates(configDir).filter((s) => s.parent === parent && s.status === "running");
    if (running.length >= maxChildren) {
      throw new Error(`父会话已有 ${running.length} 个子 agent 在跑（上限 ${maxChildren}）；先 wait 或停止一个`);
    }

    const repo = repoRoot(cwd);
    if (isolate && !repo) throw new Error(`${cwd} 不是 git 仓库；传 isolate:false 在当前目录跑（不隔离）`);
    const parentDirty = repo ? repoDirty(repo) : { dirty: false, files: [] };

    let child = newChildId();
    while (existsSync(delegationDir(configDir, child))) child = newChildId();
    const wt = isolate ? worktreePathFor({ configDir, child }) : cwd;
    const branch = isolate ? branchName(parent, child) : null;
    const base = repo ? repoHead(repo) : null;
    writeTask(configDir, child, text);
    writeChildState(configDir, child, {
      child,
      parent,
      cwd,
      worktree: wt,
      branch,
      baseCommit: base,
      isolated: Boolean(isolate),
      repo,
      status: "running",
      prompted: false,
      promptDone: false,
      turns: 0,
      task: text.slice(0, 200),
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
      bin,
      node,
    });

    try {
      if (isolate) createWorktree({ repo, path: wt, branch, base });
      await startSupervisor({
        configDir, id: child, cwd: wt, bin, node,
        kind: "child", taskFile: taskPath(configDir, child), parent,
      });
    } catch (e) {
      if (isolate) {
        try { removeWorktree({ repo, path: wt, branch }); } catch (e2) { /* ignore */ }
      }
      patchChildState(configDir, child, { status: "error", error: e.message });
      throw e;
    }

    return {
      session: child,
      status: "running",
      worktree: wt,
      branch,
      baseCommit: base,
      isolated: Boolean(isolate),
      parentDirty: Boolean(parentDirty.dirty),
      parentDirtyFiles: parentDirty.files?.length || 0,
    };
  }

  async function wait({ child, timeoutMs = 600000 }) {
    if (!validSessionId(child)) throw new Error(`无效子 session: ${child}`);
    const started = Date.now();
    const deadline = started + Math.max(1000, Math.min(Number(timeoutMs) || 600000, 3600000));
    for (;;) {
      const state = readChildState(configDir, child);
      if (!state) throw new Error(`找不到子 session: ${child}`);
      const result = readChildResult(configDir, child);
      if (result) return { ...result, session: child, running: false };
      if (state.status !== "running") {
        return {
          session: child, status: state.status, running: false,
          text: "", error: state.error || null,
          worktree: state.worktree, branch: state.branch, commit: null,
        };
      }
      if (Date.now() >= deadline) {
        return {
          session: child, status: "running", running: true,
          text: "", worktree: state.worktree, branch: state.branch,
          elapsedMs: Date.now() - started,
        };
      }
      await sleep(500);
    }
  }

  async function send({ child, message }) {
    if (!validSessionId(child)) throw new Error(`无效子 session: ${child}`);
    const state = readChildState(configDir, child);
    if (!state) throw new Error(`找不到子 session: ${child}`);
    const text = String(message || "").trim();
    if (!text) throw new Error("message 不能为空");
    appendInbox(configDir, child, text);
    if (state.status !== "running") {
      // 上一轮已结束；先清掉可能残留的 tmux/watchdog，再以同一 session 续跑。
      if (stopSupervisor) {
        try { await stopSupervisor({ configDir, id: child }); } catch (e) { /* ignore */ }
      }
      patchChildState(configDir, child, { status: "running", promptDone: false, error: null });
      await startSupervisor({
        configDir, id: child, cwd: state.worktree || state.cwd,
        bin: state.bin ?? bin, node: state.node ?? node,
        kind: "child", taskFile: taskPath(configDir, child), parent: state.parent,
      });
      return { session: child, status: "resumed" };
    }
    return { session: child, status: "queued" };
  }

  function list({ parent = null } = {}) {
    return listDelegationStates(configDir)
      .filter((s) => !parent || s.parent === parent)
      .map((s) => {
        const r = readChildResult(configDir, s.child);
        return {
          session: s.child,
          parent: s.parent,
          status: r?.status || s.status,
          task: String(s.task || readTask(configDir, s.child)).slice(0, 120),
          worktree: s.worktree,
          branch: s.branch,
          commit: r?.commit || null,
          cost: r?.cost || 0,
          createdAt: s.createdAt,
        };
      });
  }

  return { spawn, wait, send, list, delegations: listDelegationStates };
}

export function createDelegateTool({ configDir, getParent, getCwd, startSupervisor, stopSupervisor, bin, node, maxChildren }) {
  const delegator = createDelegator({ configDir, startSupervisor, stopSupervisor, bin, node, maxChildren });

  return {
    name: "delegate",
    description: [
      "把一个独立任务交给一个完整的 celagent 子 session。子 agent 有自己的 git worktree、lease、journal、checkpoint；父会话关闭或崩溃后它继续跑，结果可再取回。",
      "spawn 立即返回 session id；wait 阻塞取结果；send 在下一个 turn 边界给子 agent 追加指令；list 看这棵树。",
      "子 agent 从 HEAD 创建 worktree，父会话未提交的改动不会带过去。子 agent 不能再 spawn，也不能 push。",
      "只把互不重叠的任务并行；改同一批文件的任务要串行。合并永远由人决定。",
    ].join(""),
    parameters: {
      type: "object",
      properties: {
        action: { type: "string", enum: ["spawn", "wait", "send", "list"], description: "要执行的动作" },
        task: { type: "string", description: "spawn: 交给子 agent 的完整任务描述" },
        session: { type: "string", description: "wait/send: spawn 返回的子 session id" },
        message: { type: "string", description: "send: 追加给子 agent 的指令" },
        isolate: { type: "boolean", description: "spawn: 是否创建独立 worktree，默认 true" },
        timeoutMs: { type: "number", description: "wait: 最长等待毫秒数，默认 600000" },
      },
      required: ["action"],
    },
    execute: async (toolCallId, params = {}) => {
      try {
        const action = String(params.action || "");
        if (action === "spawn") {
          const parent = getParent();
          if (!parent) return { content: [{ type: "text", text: "✗ 当前会话没有持久化 ID，无法 spawn" }] };
          const r = await delegator.spawn({ parent, cwd: getCwd(), task: params.task, isolate: params.isolate !== false });
          const notes = [`已派出子 agent ${r.session}。`];
          notes.push(`worktree: ${r.worktree}`);
          if (r.branch) notes.push(`branch: ${r.branch}`);
          if (r.baseCommit) notes.push(`base: ${r.baseCommit}`);
          if (r.parentDirty) notes.push(`注意: 父会话有 ${r.parentDirtyFiles} 个未提交文件，子 agent 从 HEAD 开始，不会包含这些改动。`);
          notes.push("需要结果时调用 delegate wait；也可以先做别的事。");
          return { content: [{ type: "text", text: notes.join("\n") }], details: r };
        }
        if (action === "wait") {
          if (!params.session) return { content: [{ type: "text", text: "✗ wait 需要 session" }] };
          const r = await delegator.wait({ child: String(params.session), timeoutMs: params.timeoutMs });
          return { content: [{ type: "text", text: formatWait(r) }], details: r };
        }
        if (action === "send") {
          if (!params.session) return { content: [{ type: "text", text: "✗ send 需要 session" }] };
          const r = await delegator.send({ child: String(params.session), message: params.message });
          return { content: [{ type: "text", text: `已投递到 ${r.session}（${r.status}）。` }], details: r };
        }
        if (action === "list") {
          const rows = delegator.list({ parent: getParent() });
          if (rows.length === 0) return { content: [{ type: "text", text: "(没有子 agent)" }] };
          const text = rows.map((r) => `- ${r.session}  ${r.status}  ${r.branch || "-"}  ${r.task}`).join("\n");
          return { content: [{ type: "text", text }], details: { children: rows } };
        }
        return { content: [{ type: "text", text: `✗ 未知 action: ${action}` }] };
      } catch (e) {
        return { content: [{ type: "text", text: `✗ delegate 失败: ${e.message}` }] };
      }
    },
  };
}
