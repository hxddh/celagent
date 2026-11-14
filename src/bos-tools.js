// bos-tools.js — v1.0 记忆工具
// history_search: 跨 v2 会话检索（manifest + segments，结果带 session/seq）
// session_snapshot: 只写 pointer（session + seq），不复制 turns
import { homedir } from "node:os";
import { join } from "node:path";
import { readFileSync, existsSync } from "node:fs";
import { resolveEndpoint, resolveRegion } from "./bos.js";
import { createBosAdapter } from "./v2/adapter.js";
import { readSession, listSessions, readManifest } from "./v2/index.js";
import { turnsFromJsonl } from "./persist.js";

export function persistenceFromCfg(cfg) {
  const bucket = cfg?.persistence?.bucket || null;
  if (!bucket) return { error: "no-bucket", message: "未配置 persistence.bucket" };
  try {
    const endpoint = resolveEndpoint(cfg.persistence?.endpoint);
    const region = resolveRegion(endpoint, cfg.persistence?.region);
    const profile = String(cfg.persistence?.profile || "").trim() || "bos";
    return { bucket, endpoint, profile, region };
  } catch (e) {
    if (e.code === "endpoint-not-allowed") return { error: "endpoint-not-allowed", message: e.message };
    return { error: "config", message: String(e.message || e) };
  }
}

export function loadPersistence() {
  const cfgFile = join(homedir(), ".config", "celagent", "settings.json");
  if (!existsSync(cfgFile)) return { error: "no-config", message: "未配置 persistence.bucket" };
  try { return persistenceFromCfg(JSON.parse(readFileSync(cfgFile, "utf8"))); }
  catch (e) { return { error: "bad-config", message: String(e.message || e) }; }
}

function persUnavailable(pers, action) {
  if (!pers || pers.error === "no-config" || pers.error === "no-bucket") return `未配置 persistence.bucket, 无法${action}`;
  if (pers.error === "endpoint-not-allowed") return pers.message || "persistence.endpoint 不允许";
  if (pers.error) return `对象存储配置无效: ${pers.message || pers.error}`;
  return null;
}

function textOf(turn) {
  const parts = [turn.msg || ""];
  if (Array.isArray(turn.content)) {
    for (const b of turn.content) {
      if (b.type === "text" && b.text) parts.push(b.text);
      if (b.type === "toolCall" && b.name) parts.push(`工具:${b.name}`);
      if (b.type === "thinking" && b.thinking) parts.push(b.thinking);
    }
  }
  if (Array.isArray(turn.toolResults)) {
    for (const tr of turn.toolResults) {
      if (tr.toolName) parts.push(`工具:${tr.toolName}`);
      if (Array.isArray(tr.content)) {
        for (const b of tr.content) if (b.type === "text" && b.text) parts.push(b.text);
      }
    }
  }
  return parts.join("\n");
}

export function collectHitsFromTurns(turns, { query, sessionId, source, hits, limit }) {
  for (const turn of turns || []) {
    const full = textOf(turn);
    const idx = full.toLowerCase().indexOf(query);
    if (idx < 0) continue;
    const start = Math.max(0, idx - 60);
    const snippet = (start > 0 ? "…" : "") + full.slice(start, start + 200);
    hits.push({ session: sessionId, turn: turn.turn, role: turn.role || "?", ts: turn.ts, snippet, source });
    if (hits.length >= limit) return true;
  }
  return false;
}

export const history_search = {
  name: "history_search",
  description: "在对象存储 v2 会话里搜索记忆。默认只搜当前会话; 传 session=\"*\" 才跨会话。返回 session/seq 引用与片段。",
  parameters: {
    type: "object",
    properties: {
      query: { type: "string", description: "搜索关键词" },
      limit: { type: "number", description: "返回条数, 默认 5, 最大 20" },
      session: { type: "string", description: "会话 ID; 默认当前会话; 传 * 搜索全部会话" },
    },
    required: ["query"],
  },
  execute: async (toolCallId, params) => {
    try {
      const pers = loadPersistence();
      const unavailable = persUnavailable(pers, "搜索历史");
      if (unavailable) return { content: [{ type: "text", text: unavailable }] };
      const query = String(params.query || "").toLowerCase();
      const limit = Math.min(Number(params.limit) || 5, 20);
      const current = typeof globalThis.__celagentPersistId === "string" ? globalThis.__celagentPersistId : null;
      const rawSession = params.session != null ? String(params.session) : current;
      const cross = rawSession === "*" || rawSession === "all";
      const filter = cross ? null : rawSession;
      if (!filter && !cross) return { content: [{ type: "text", text: "未指定会话。默认只搜当前会话; 跨会话请传 session=\"*\"" }] };
      if (filter && !/^[A-Za-z0-9._-]{1,128}$/.test(filter)) return { content: [{ type: "text", text: "非法会话 ID" }] };

      const store = createBosAdapter(pers);
      const hits = [];
      let scanned = 0;
      let readError = null;
      const ids = filter ? [filter] : (await listSessions(store)).sessions.slice(0, 20).map((s) => s.id);
      for (const id of ids) {
        try {
          const s = await readSession(store, id);
          if (!s) continue;
          scanned += 1;
          if (collectHitsFromTurns(turnsFromJsonl(s.text), { query, sessionId: id, source: "session", hits, limit })) break;
        } catch (e) {
          readError = readError || `${id}: ${e.message}`;
        }
      }
      if (hits.length === 0) {
        return { content: [{ type: "text", text: `在 ${scanned} 个 v2 会话中未找到与"${params.query}"相关的内容${readError ? `\n\n(警告: 部分会话读取失败: ${readError})` : ""}` }] };
      }
      const lines = hits.map((h) => `[会话 ${h.session} · #${h.turn} · ${h.role}${h.ts ? " · " + new Date(h.ts).toLocaleString() : ""}]\n  ${h.snippet.replace(/\n/g, " ")}`);
      return { content: [{ type: "text", text: `找到 ${hits.length} 条相关记忆:\n\n` + lines.join("\n\n") + (readError ? `\n\n(警告: 部分会话读取失败: ${readError})` : "") }] };
    } catch (e) {
      return { content: [{ type: "text", text: `history_search 失败: ${e.message}` }] };
    }
  },
};

export const session_snapshot = {
  name: "session_snapshot",
  description: "把当前会话的当前位置保存为 pointer（session + seq），不复制 turns。之后可用 session/seq 恢复或引用。",
  parameters: {
    type: "object",
    properties: {
      name: { type: "string", description: "快照名称" },
      note: { type: "string", description: "备注(可选)" },
    },
    required: ["name"],
  },
  execute: async (toolCallId, params) => {
    try {
      const pers = loadPersistence();
      const unavailable = persUnavailable(pers, "保存快照");
      if (unavailable) return { content: [{ type: "text", text: unavailable }] };
      const name = String(params.name || "").trim();
      if (!name) return { content: [{ type: "text", text: "缺少快照名称" }] };
      const sessionId = typeof globalThis.__celagentPersistId === "string" ? globalThis.__celagentPersistId : null;
      if (!sessionId) return { content: [{ type: "text", text: "没有当前会话 ID，无法保存快照" }] };
      const store = createBosAdapter(pers);
      const manifest = await readManifest(store, sessionId);
      if (!manifest) return { content: [{ type: "text", text: `会话 ${sessionId} 没有 v2 manifest` }] };
      const key = `snapshots/${name.replace(/[^\w\u4e00-\u9fa5-]/g, "_")}-${Date.now()}.json`;
      const body = {
        v: 1, name, note: params.note ? String(params.note) : "",
        session: sessionId, seq: manifest.head.seq, entryId: manifest.head.entryId,
        createdAt: Date.now(),
      };
      const put = await store.put(key, JSON.stringify(body));
      if (!put.ok) return { content: [{ type: "text", text: `快照保存失败: ${put.error || "未知错误"}` }] };
      return { content: [{ type: "text", text: `已保存快照 pointer: ${key} (session=${sessionId}, seq=${body.seq})` }] };
    } catch (e) {
      return { content: [{ type: "text", text: `session_snapshot 失败: ${e.message}` }] };
    }
  },
};
