// bos-tools.js — P1 记忆增强: agent 可用的 BOS 记忆工具
// history_search: 跨会话检索历史记忆 (只读)
// session_snapshot: 显式记忆锚点 (写 snapshots/ 前缀, 不碰权威数据)
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { readFileSync, existsSync } from "node:fs";
import { execFile } from "node:child_process";
import { writeFile, readFile, chmod, mkdtemp, rm } from "node:fs/promises";
import { awsEnv, resolveEndpoint } from "./bos.js";

function runAws(args) {
  return new Promise((resolve) => {
    execFile("aws", args, { env: awsEnv(), timeout: 20000, encoding: "utf8" }, (err, stdout) => {
      try { resolve(JSON.parse(stdout || "[]")); }
      catch (e) { resolve([]); }
    });
  });
}

function loadPersistence() {
  const cfgFile = join(homedir(), ".config", "celagent", "settings.json");
  if (!existsSync(cfgFile)) return null;
  try {
    const cfg = JSON.parse(readFileSync(cfgFile, "utf8"));
    const bucket = cfg.persistence?.bucket || null;
    if (!bucket) return null;
    return { bucket, endpoint: resolveEndpoint(cfg.persistence?.endpoint) };
  } catch (e) { return null; }
}

async function privateTmp(name = "body.json") {
  const dir = await mkdtemp(join(tmpdir(), "celagent-"));
  try { await chmod(dir, 0o700); } catch (e) { /* ignore */ }
  return {
    dir,
    path: join(dir, name),
    async cleanup() {
      try { await rm(dir, { recursive: true, force: true }); } catch (e) { /* ignore */ }
    },
  };
}

function textOf(turn) {
  // 从 turn 提取可搜索文本: msg + content 文本块 + 工具名
  const parts = [turn.msg || ""];
  if (Array.isArray(turn.content)) {
    for (const b of turn.content) {
      if (b.type === "text" && b.text) parts.push(b.text);
      if (b.type === "toolCall" && b.name) parts.push(`工具:${b.name}`);
      if (b.type === "thinking" && b.thinking) parts.push(b.thinking);
    }
  }
  return parts.join("\n");
}

// ---- 工具 1: history_search — 跨会话检索记忆 ----
export const history_search = {
  name: "history_search",
  description: "在 BOS 云端历史中搜索记忆。默认只搜当前会话; 传 session=\"*\" 才跨会话。返回匹配轮次片段。",
  parameters: {
    type: "object",
    properties: {
      query: { type: "string", description: "搜索关键词(如: 并发、bug、架构)" },
      limit: { type: "number", description: "返回条数, 默认 5, 最大 20" },
      session: { type: "string", description: "会话 ID; 默认当前会话; 传 * 搜索全部会话" },
    },
    required: ["query"],
  },
  execute: async (toolCallId, params) => {
    try {
      const pers = loadPersistence();
      if (!pers) return { content: [{ type: "text", text: "未配置 BOS bucket, 无法搜索历史" }] };
      const { bucket, endpoint } = pers;
      const query = String(params.query || "").toLowerCase();
      const limit = Math.min(Number(params.limit) || 5, 20);
      const persistId = typeof globalThis.__celagentPersistId === "string" ? globalThis.__celagentPersistId : null;
      const rawSession = params.session != null ? String(params.session) : persistId;
      const cross = rawSession === "*" || rawSession === "all";
      const sessionFilter = cross ? null : rawSession;
      if (!sessionFilter && !cross) {
        return { content: [{ type: "text", text: "未指定会话。默认只搜当前会话; 跨会话请传 session=\"*\"" }] };
      }
      if (sessionFilter && !/^[A-Za-z0-9._-]{1,128}$/.test(sessionFilter)) {
        return { content: [{ type: "text", text: "非法会话 ID" }] };
      }

      let keys;
      if (sessionFilter) {
        keys = [`sessions/${sessionFilter}.json`];
      } else {
        keys = await runAws(["s3api", "list-objects-v2", "--bucket", bucket, "--prefix", "sessions/", "--endpoint-url", endpoint, "--max-items", "40", "--query", "Contents[].Key", "--output", "json"]);
        if (!Array.isArray(keys)) keys = [];
        keys = keys.filter((k) => typeof k === "string").slice(0, 40);
      }

      const hits = [];
      for (const key of keys) {
        const tmp = await privateTmp("search.json");
        try {
          const dl = await new Promise((resolve) => {
            execFile("aws", ["s3api", "get-object", "--bucket", bucket, "--key", key, "--endpoint-url", endpoint, tmp.path], { env: awsEnv(), timeout: 15000 }, (err) => resolve(!err));
          });
          if (!dl) continue;
          try { await chmod(tmp.path, 0o600); } catch (e) { /* ignore */ }
          try {
            const session = JSON.parse(await readFile(tmp.path, "utf8"));
            const sessionId = key.replace("sessions/", "").replace(".json", "");
            for (const turn of (session.turns || [])) {
              const haystack = textOf(turn).toLowerCase();
              if (haystack.includes(query)) {
                const snippet = (turn.msg || textOf(turn)).slice(0, 200);
                hits.push({ session: sessionId, turn: turn.turn, role: turn.role || "?", ts: turn.ts, snippet });
                if (hits.length >= limit) break;
              }
            }
          } catch (e) { /* 跳过损坏会话 */ }
        } finally {
          await tmp.cleanup();
        }
        if (hits.length >= limit) break;
      }

      if (hits.length === 0) {
        return { content: [{ type: "text", text: `在 ${keys.length} 个历史会话中未找到与"${params.query}"相关的内容` }] };
      }
      const lines = hits.map(h =>
        `[会话 ${h.session} · 第${h.turn}轮 · ${h.role}${h.ts ? " · " + new Date(h.ts).toLocaleString() : ""}]\n  ${h.snippet.replace(/\n/g, " ")}`
      );
      return { content: [{ type: "text", text: `找到 ${hits.length} 条相关记忆:\n\n` + lines.join("\n\n") }] };
    } catch (e) {
      return { content: [{ type: "text", text: `history_search 失败: ${e.message}` }] };
    }
  },
};

// ---- 工具 2: session_snapshot — 显式记忆锚点 ----
export const session_snapshot = {
  name: "session_snapshot",
  description: "将当前会话的关键状态保存为 BOS 快照(显式记忆锚点)。用于在重要节点主动保存,之后可用 celagent export 或恢复时引用。参数: name 必填(快照名称, 如 '架构决策-20260810'), 可选 note(备注说明)。",
  parameters: {
    type: "object",
    properties: {
      name: { type: "string", description: "快照名称(如: 并发修复方案)" },
      note: { type: "string", description: "备注(可选)" },
    },
    required: ["name"],
  },
  execute: async (toolCallId, params) => {
    try {
      const pers = loadPersistence();
      if (!pers) return { content: [{ type: "text", text: "未配置 BOS bucket, 无法保存快照" }] };
      const { bucket, endpoint } = pers;
      const name = String(params.name || "").trim();
      if (!name) return { content: [{ type: "text", text: "缺少快照名称" }] };
      const note = params.note ? String(params.note) : "";
      const currentTurns = (typeof globalThis.__celagentSnapshotTurns === "function")
        ? globalThis.__celagentSnapshotTurns()
        : [];
      const key = `snapshots/${name.replace(/[^\w\u4e00-\u9fa5-]/g, "_")}-${Date.now()}.json`;
      const body = JSON.stringify({
        name, note, createdAt: Date.now(),
        turns: currentTurns,
      });
      const tmp = await privateTmp("snap.json");
      try {
        await writeFile(tmp.path, body, { encoding: "utf8", mode: 0o600 });
        const put = await new Promise((resolve) => {
          execFile("aws", ["s3api", "put-object", "--bucket", bucket, "--key", key, "--body", tmp.path, "--endpoint-url", endpoint, "--output", "json"], { env: awsEnv(), timeout: 20000 }, (err, stdout) => resolve(!err));
        });
        if (!put) return { content: [{ type: "text", text: "快照保存失败" }] };
        return { content: [{ type: "text", text: `已保存会话快照: ${key} (${currentTurns.length} 轮)` }] };
      } finally {
        await tmp.cleanup();
      }
    } catch (e) {
      return { content: [{ type: "text", text: `session_snapshot 失败: ${e.message}` }] };
    }
  },
};
