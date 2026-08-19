// bos-tools.js — P1 记忆增强: agent 可用的对象存储记忆工具
// history_search: 跨会话检索历史记忆 (只读; sessions/ + snapshots/)
// session_snapshot: 显式记忆锚点 (写 snapshots/ 前缀, 不碰权威数据)
import { homedir } from "node:os";
import { join } from "node:path";
import { readFileSync, existsSync } from "node:fs";
import { awsJson, bosGet, bosPut, resolveEndpoint, resolveRegion } from "./bos.js";
import { isJsonlBody, turnsFromJsonl } from "./persist.js";

export function persistenceFromCfg(cfg) {
  const bucket = cfg?.persistence?.bucket || null;
  if (!bucket) return { error: "no-bucket", message: "未配置 persistence.bucket" };
  try {
    const endpoint = resolveEndpoint(cfg.persistence?.endpoint);
    const region = resolveRegion(endpoint, cfg.persistence?.region);
    const profile = String(cfg.persistence?.profile || "").trim() || "bos";
    return { bucket, endpoint, profile, region };
  } catch (e) {
    if (e.code === "endpoint-not-allowed") {
      return { error: "endpoint-not-allowed", message: e.message };
    }
    return { error: "config", message: String(e.message || e) };
  }
}

export function loadPersistence() {
  const cfgFile = join(homedir(), ".config", "celagent", "settings.json");
  if (!existsSync(cfgFile)) return { error: "no-config", message: "未配置 persistence.bucket" };
  try {
    return persistenceFromCfg(JSON.parse(readFileSync(cfgFile, "utf8")));
  } catch (e) {
    return { error: "bad-config", message: String(e.message || e) };
  }
}

function persUnavailable(pers, action) {
  if (!pers || pers.error === "no-config" || pers.error === "no-bucket") {
    return `未配置 persistence.bucket, 无法${action}`;
  }
  if (pers.error === "endpoint-not-allowed") {
    return pers.message || "persistence.endpoint 不允许";
  }
  if (pers.error) {
    return `对象存储配置无效: ${pers.message || pers.error}`;
  }
  return null;
}

function textOf(turn) {
  // 从 turn 提取可搜索文本: msg + content 文本块 + 工具名/结果
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
        for (const b of tr.content) {
          if (b.type === "text" && b.text) parts.push(b.text);
        }
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
    // 片段取匹配位置附近 — 命中可能在 toolResults/thinking 深处, turn.msg 开头未必含匹配文本
    const start = Math.max(0, idx - 60);
    const snippet = (start > 0 ? "…" : "") + full.slice(start, start + 200);
    hits.push({ session: sessionId, turn: turn.turn, role: turn.role || "?", ts: turn.ts, snippet, source });
    if (hits.length >= limit) return true;
  }
  return false;
}

function sessionIdFromKey(key) {
  return String(key || "").replace(/^sessions\//, "").replace(/\.jsonl$/i, "").replace(/\.json$/i, "");
}

function turnsFromSessionObject(key, body) {
  if (String(key).endsWith(".jsonl") || isJsonlBody(body)) {
    if (!isJsonlBody(body)) return null;
    return { id: sessionIdFromKey(key), turns: turnsFromJsonl(body) };
  }
  try {
    const session = JSON.parse(body);
    return { id: session.id || sessionIdFromKey(key), turns: session.turns || [] };
  } catch (e) {
    return null;
  }
}

async function listKeys(pers, prefix) {
  const r = await awsJson([
    "s3api", "list-objects-v2",
    "--bucket", pers.bucket,
    "--prefix", prefix,
    "--endpoint-url", pers.endpoint,
    "--max-items", "40",
    "--query", "Contents[].Key",
    "--output", "json",
  ], { profile: pers.profile, region: pers.region });
  if (!r.ok) return r;
  const keys = Array.isArray(r.data) ? r.data.filter((k) => typeof k === "string").slice(0, 40) : [];
  return { ok: true, keys };
}

// ---- 工具 1: history_search — 跨会话检索记忆 ----
export const history_search = {
  name: "history_search",
  description: "在对象存储历史中搜索记忆(sessions/ 与 snapshots/)。默认只搜当前会话; 传 session=\"*\" 才跨会话。返回匹配轮次片段。",
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
      const unavailable = persUnavailable(pers, "搜索历史");
      if (unavailable) return { content: [{ type: "text", text: unavailable }] };
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

      let sessionKeys;
      if (sessionFilter) {
        sessionKeys = [`sessions/${sessionFilter}.jsonl`, `sessions/${sessionFilter}.json`];
      } else {
        const listed = await listKeys(pers, "sessions/");
        if (!listed.ok) return { content: [{ type: "text", text: `列举会话失败: ${listed.error}` }] };
        sessionKeys = listed.keys
          .filter((k) => k.endsWith(".jsonl") || k.endsWith(".json"))
          .sort((a, b) => Number(b.endsWith(".jsonl")) - Number(a.endsWith(".jsonl")));
      }

      const snapListed = await listKeys(pers, "snapshots/");
      if (!snapListed.ok) return { content: [{ type: "text", text: `列举快照失败: ${snapListed.error}` }] };
      const snapKeys = snapListed.keys;

      const hits = [];
      const storeOpts = { bucket: pers.bucket, endpoint: pers.endpoint, profile: pers.profile, region: pers.region };
      let sessionReadError = null;
      const haveJsonl = new Set();
      for (const key of sessionKeys) {
        const sid = sessionIdFromKey(key);
        const jsonlKey = key.endsWith(".jsonl");
        if (!jsonlKey && haveJsonl.has(sid)) continue;
        const got = await bosGet(key, storeOpts);
        if (!got.ok) {
          const miss = got.error === "not-found" || /\bNoSuchKey\b|\b404\b/i.test(String(got.error || ""));
          // 读失败 ≠ 已覆盖: 不标 haveJsonl (否则旧 .json 回退被跳过), 且跨会话搜索也要上报部分失败
          if (!miss) sessionReadError = sessionReadError || got.error;
          continue;
        }
        const parsed = turnsFromSessionObject(key, got.body);
        if (!parsed) continue;
        if (jsonlKey) haveJsonl.add(parsed.id);
        if (collectHitsFromTurns(parsed.turns, { query, sessionId: parsed.id, source: "session", hits, limit })) break;
      }
      // 会话对象读失败不在此早退 — 快照可能仍有该会话的命中, 先扫完再决定
      if (hits.length < limit) {
        for (const key of snapKeys) {
          const got = await bosGet(key, storeOpts);
          if (!got.ok) continue;
          try {
            const snap = JSON.parse(got.body);
            const snapSession = snap.session != null ? String(snap.session) : "";
            if (sessionFilter && snapSession && snapSession !== sessionFilter) continue;
            if (sessionFilter && !snapSession) continue;
            const label = snap.name ? `snapshot:${snap.name}` : key.replace("snapshots/", "").replace(/\.json$/, "");
            if (collectHitsFromTurns(snap.turns, { query, sessionId: label, source: "snapshot", hits, limit })) break;
          } catch (e) { /* 跳过损坏快照 */ }
        }
      }

      const scanned = sessionKeys.length + snapKeys.length;
      const readWarn = sessionReadError
        ? `\n\n(警告: 部分会话对象读取失败: ${sessionReadError}; 结果可能不完整)` : "";
      if (hits.length === 0) {
        if (sessionFilter && sessionReadError) {
          return { content: [{ type: "text", text: `读取会话失败: ${sessionReadError}; 已扫描 ${snapKeys.length} 个快照, 未找到与"${params.query}"相关的内容` }] };
        }
        return { content: [{ type: "text", text: `在 ${scanned} 个历史对象(会话+快照)中未找到与"${params.query}"相关的内容${readWarn}` }] };
      }
      const lines = hits.map(h => {
        const kind = h.source === "snapshot" ? "快照" : "会话";
        return `[${kind} ${h.session} · 第${h.turn}轮 · ${h.role}${h.ts ? " · " + new Date(h.ts).toLocaleString() : ""}]\n  ${h.snippet.replace(/\n/g, " ")}`;
      });
      return { content: [{ type: "text", text: `找到 ${hits.length} 条相关记忆:\n\n` + lines.join("\n\n") + readWarn }] };
    } catch (e) {
      return { content: [{ type: "text", text: `history_search 失败: ${e.message}` }] };
    }
  },
};

// ---- 工具 2: session_snapshot — 显式记忆锚点 ----
export const session_snapshot = {
  name: "session_snapshot",
  description: "将当前会话的关键状态保存为对象存储快照(显式记忆锚点)。用于在重要节点主动保存,之后可用 celagent export 或恢复时引用。参数: name 必填(快照名称, 如 '架构决策-20260810'), 可选 note(备注说明)。",
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
      const unavailable = persUnavailable(pers, "保存快照");
      if (unavailable) return { content: [{ type: "text", text: unavailable }] };
      const name = String(params.name || "").trim();
      if (!name) return { content: [{ type: "text", text: "缺少快照名称" }] };
      const note = params.note ? String(params.note) : "";
      // getter 可能是异步的 (TUI 从 BOS 权威源重建全量轮次), await 兼容同步/异步实现
      const currentTurns = (typeof globalThis.__celagentSnapshotTurns === "function")
        ? (await globalThis.__celagentSnapshotTurns()) || []
        : [];
      const persistId = typeof globalThis.__celagentPersistId === "string" ? globalThis.__celagentPersistId : null;
      const key = `snapshots/${name.replace(/[^\w\u4e00-\u9fa5-]/g, "_")}-${Date.now()}.json`;
      const put = await bosPut(key, {
        name, note, createdAt: Date.now(),
        session: persistId,
        turns: currentTurns,
      }, { bucket: pers.bucket, endpoint: pers.endpoint, profile: pers.profile, region: pers.region });
      if (!put.ok) return { content: [{ type: "text", text: `快照保存失败: ${put.error || "未知错误"}` }] };
      return { content: [{ type: "text", text: `已保存会话快照: ${key} (${currentTurns.length} 轮)` }] };
    } catch (e) {
      return { content: [{ type: "text", text: `session_snapshot 失败: ${e.message}` }] };
    }
  },
};
