// v2/journal.js — 工具执行 journal
// 先写本地 pending（fsync），再写远端；tool_call 失败即 block，tool_result 失败留本地待补。
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync, renameSync, unlinkSync, readdirSync, openSync, fsyncSync, closeSync } from "node:fs";
import { join, dirname } from "node:path";
import { journalKey } from "./keys.js";

function atomicWrite(path, text) {
  mkdirSync(dirname(path), { recursive: true });
  const tmp = `${path}.tmp-${process.pid}-${Date.now()}`;
  const fd = openSync(tmp, "w", 0o600);
  try {
    writeFileSync(fd, text, "utf8");
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
  renameSync(tmp, path);
}

function safeId(toolCallId) {
  return String(toolCallId).replace(/[^A-Za-z0-9._-]/g, "_").slice(0, 96) || "unknown";
}

export function createJournal({ store, localDir, now = Date.now }) {
  mkdirSync(localDir, { recursive: true, mode: 0o700 });

  function localPath(sessionId, toolCallId) {
    return join(localDir, sessionId, `${safeId(toolCallId)}.json`);
  }

  async function readRemote(sessionId, toolCallId) {
    const r = await store.get(journalKey(sessionId, toolCallId));
    if (!r.ok) return { body: null, etag: null, error: r.error };
    try { return { body: JSON.parse(r.body), etag: r.etag }; }
    catch (e) { return { body: null, etag: null, error: `corrupt: ${e.message}` }; }
  }

  async function start(sessionId, { toolCallId, toolName, input }) {
    const body = {
      v: 1, session: sessionId, toolCallId, toolName,
      input: input ?? null, startedAt: now(), status: "started",
    };
    atomicWrite(localPath(sessionId, toolCallId), JSON.stringify(body) + "\n");
    const r = await store.put(journalKey(sessionId, toolCallId), JSON.stringify(body), { ifNoneMatch: true });
    if (r.ok) return true;
    if (r.conflict) {
      const existing = await readRemote(sessionId, toolCallId);
      if (existing.body && existing.body.toolCallId === toolCallId) return true;
    }
    return false;
  }

  async function finish(sessionId, { toolCallId, result, isError }) {
    let local = null;
    try { local = JSON.parse(readFileSync(localPath(sessionId, toolCallId), "utf8")); } catch (e) { local = null; }
    const body = {
      ...(local || { v: 1, session: sessionId, toolCallId, toolName: "unknown", startedAt: now(), status: "started" }),
      result: result ?? null,
      isError: Boolean(isError),
      finishedAt: now(),
      status: "finished",
    };
    for (let attempt = 0; attempt < 4; attempt++) {
      const cur = await readRemote(sessionId, toolCallId);
      const opts = cur.etag ? { ifMatch: cur.etag } : { ifNoneMatch: true };
      const r = await store.put(journalKey(sessionId, toolCallId), JSON.stringify(body), opts);
      if (r.ok) {
        try { unlinkSync(localPath(sessionId, toolCallId)); } catch (e) { /* already gone */ }
        return true;
      }
      if (!r.conflict) return false;
    }
    return false;
  }

  async function reconcile(sessionId) {
    const dir = join(localDir, sessionId);
    let files = [];
    try { files = readdirSync(dir).filter((f) => f.endsWith(".json")); } catch (e) { return []; }
    const out = [];
    for (const f of files) {
      let local;
      try { local = JSON.parse(readFileSync(join(dir, f), "utf8")); } catch (e) { continue; }
      const remote = await readRemote(sessionId, local.toolCallId);
      if (remote.body?.status === "finished") {
        try { unlinkSync(join(dir, f)); } catch (e) { /* ignore */ }
        continue;
      }
      out.push({
        toolCallId: local.toolCallId,
        toolName: local.toolName,
        startedAt: local.startedAt,
        status: remote.body?.status || "started",
        unknown: !remote.body || remote.body.status !== "finished",
      });
    }
    return out;
  }

  async function list(sessionId) {
    const prefix = `sessions/${sessionId}/journal/`;
    const r = await store.list(prefix);
    if (!r.ok) return [];
    const out = [];
    for (const item of r.keys.slice(0, 200)) {
      const g = await store.get(item.key);
      if (!g.ok) continue;
      try { out.push(JSON.parse(g.body)); } catch (e) { /* skip */ }
    }
    return out;
  }

  return { start, finish, reconcile, list, localPath, readRemote };
}

export function journalIdFor(toolCallId) {
  return createHash("sha256").update(String(toolCallId)).digest("hex").slice(0, 16);
}
