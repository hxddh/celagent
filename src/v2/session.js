// v2/session.js — 分段 Pi JSONL 会话
// 语义源仍是 Pi 的 v3 JSONL；按 entry 分段，manifest CAS 提交，读时按 hash 校验。
import { createHash } from "node:crypto";
import { existsSync, readFileSync, writeFileSync, renameSync, mkdirSync, unlinkSync } from "node:fs";
import { dirname } from "node:path";
import { sanitizeLocalJsonl } from "../persist.js";
import { createManifest, readManifest, writeManifest } from "./manifest.js";
import {
  manifestKey, segmentKey, sessionPrefix, legacyJsonlKey, legacyTurnsKey,
} from "./keys.js";
import { jsonlFromTurns } from "../persist.js";

export const SEGMENT_MAX_ENTRIES = 256;
export const SEGMENT_MAX_BYTES = 4 * 1024 * 1024;

export class InvalidJsonlError extends Error {
  constructor(detail) {
    super(`本地会话不是合法的 Pi JSONL: ${detail}`);
    this.name = "InvalidJsonlError";
    this.code = "invalid-jsonl";
  }
}

export class LineageMismatchError extends Error {
  constructor(sessionId, detail) {
    super(`会话 ${sessionId} 与远端谱系不一致: ${detail}`);
    this.name = "LineageMismatchError";
    this.code = "lineage-mismatch";
    this.sessionId = sessionId;
  }
}

export function sha256(text) {
  return createHash("sha256").update(String(text)).digest("hex");
}
export function hashLine(line) {
  return `sha256:${sha256(line)}`;
}

export function parseLocalJsonl(text) {
  const raw = String(text ?? "");
  const lines = raw.split(/\r?\n/).filter((l) => l.length > 0);
  if (lines.length === 0) throw new InvalidJsonlError("空文件");
  let header;
  try { header = JSON.parse(lines[0]); } catch (e) { throw new InvalidJsonlError(`header: ${e.message}`); }
  if (!header || header.type !== "session") throw new InvalidJsonlError("首行不是 session header");
  const entries = [];
  for (let i = 1; i < lines.length; i++) {
    let obj;
    try { obj = JSON.parse(lines[i]); } catch (e) { throw new InvalidJsonlError(`第 ${i + 1} 行: ${e.message}`); }
    if (!obj || typeof obj.id !== "string" || !obj.id) {
      throw new InvalidJsonlError(`第 ${i + 1} 行缺少 entry id`);
    }
    entries.push({ obj, id: obj.id, line: lines[i], parentId: obj.parentId ?? null });
  }
  return { header, entries };
}

export function renderJsonl(header, entries) {
  const head = JSON.stringify(header) + "\n";
  if (!entries || entries.length === 0) return head;
  return head + entries.map((e) => e.line).join("\n") + "\n";
}

function readLocalFile(localFile) {
  if (!existsSync(localFile)) return null;
  const raw = readFileSync(localFile, "utf8");
  const clean = sanitizeLocalJsonl(raw);
  if (!clean) throw new InvalidJsonlError(localFile);
  return clean;
}

function chunkEntries(entries, maxEntries = SEGMENT_MAX_ENTRIES, maxBytes = SEGMENT_MAX_BYTES) {
  const chunks = [];
  let current = [];
  let bytes = 0;
  for (const entry of entries) {
    const size = Buffer.byteLength(entry.line) + 1;
    if (current.length > 0 && (current.length >= maxEntries || bytes + size > maxBytes)) {
      chunks.push(current);
      current = [];
      bytes = 0;
    }
    current.push(entry);
    bytes += size;
  }
  if (current.length) chunks.push(current);
  return chunks;
}

function localPrefixMatches(manifest, local) {
  if (manifest.head.seq === 0) return true;
  if (local.entries.length < manifest.head.seq) return false;
  const expected = [];
  for (const seg of manifest.segments) {
    if (Array.isArray(seg.entryIds)) expected.push(...seg.entryIds);
  }
  if (expected.length === manifest.head.seq) {
    for (let i = 0; i < expected.length; i++) {
      if (local.entries[i]?.id !== expected[i]) return false;
    }
  } else {
    // 旧 v2 manifest 没有 entryIds：至少校验 head
    const e = local.entries[manifest.head.seq - 1];
    if (!e || e.id !== manifest.head.entryId) return false;
  }
  const last = local.entries[manifest.head.seq - 1];
  return Boolean(last && hashLine(last.line) === manifest.head.hash);
}

async function ensureManifest(store, sessionId, header) {
  let manifest = await readManifest(store, sessionId);
  if (manifest) return manifest;
  const fresh = createManifest(sessionId, header);
  const written = await writeManifest(store, fresh);
  if (written.ok) return written.manifest;
  if (written.conflict) {
    manifest = await readManifest(store, sessionId);
    if (manifest) return manifest;
  }
  throw new Error(`创建 manifest 失败: ${written.error || "conflict"}`);
}

/**
 * 把本地 Pi JSONL 的新 entry 追加为不可变 segment，然后 CAS manifest。
 * 返回最新 manifest；无新 entry 时返回现有 manifest。
 */
export async function appendFromFile(store, sessionId, localFile, opts = {}) {
  const lease = opts.lease;
  if (lease) await lease.assert(sessionId);
  const clean = readLocalFile(localFile);
  if (!clean) return null;
  if (clean.repaired) writeFileSync(localFile, clean.body, { encoding: "utf8", mode: 0o600 });
  const local = parseLocalJsonl(clean.body);
  let manifest = await ensureManifest(store, sessionId, local.header);
  if (manifest.header?.type !== "session") throw new LineageMismatchError(sessionId, "manifest 缺少 header");

  for (let attempt = 0; attempt < 6; attempt++) {
    if (manifest.head.seq > 0 && !localPrefixMatches(manifest, local)) {
      if (local.entries.length < manifest.head.seq) {
        throw new LineageMismatchError(sessionId, `本地只有 ${local.entries.length} 条，远端 head=${manifest.head.seq}`);
      }
      throw new LineageMismatchError(sessionId, "本地 entry 与远端 head 不一致（被重写/另一条谱系）");
    }
    const newEntries = local.entries.slice(manifest.head.seq);
    if (newEntries.length === 0) return manifest;

    const chunks = chunkEntries(newEntries);
    const next = { ...manifest, segments: manifest.segments.slice(), head: { ...manifest.head } };
    for (const chunk of chunks) {
      const fromSeq = next.head.seq + 1;
      const toSeq = fromSeq + chunk.length - 1;
      const body = chunk.map((e) => e.line).join("\n") + "\n";
      const hash = `sha256:${sha256(body)}`;
      const key = segmentKey(sessionId, fromSeq, toSeq, hash);
      const put = await store.put(key, body, { ifNoneMatch: true });
      if (!put.ok && !put.conflict) throw new Error(`写 segment 失败: ${put.error}`);
      if (put.conflict) {
        const existing = await store.get(key);
        if (!existing.ok || `sha256:${sha256(existing.body)}` !== hash) {
          throw new Error(`segment 冲突且内容不一致: ${key}`);
        }
      }
      const last = chunk[chunk.length - 1];
      next.segments.push({
        fromSeq, toSeq, key, hash,
        bytes: Buffer.byteLength(body),
        entries: chunk.length,
        firstEntryId: chunk[0].id,
        lastEntryId: last.id,
        entryIds: chunk.map((e) => e.id),
      });
      next.head = { seq: toSeq, entryId: last.id, hash: hashLine(last.line) };
    }
    next.updatedAt = new Date(opts.now ? opts.now() : Date.now()).toISOString();
    const written = await writeManifest(store, next, manifest.etag);
    if (written.ok) return written.manifest;
    if (written.conflict) {
      manifest = await readManifest(store, sessionId);
      if (!manifest) throw new Error("manifest 在提交冲突后消失");
      continue;
    }
    throw new Error(`提交 manifest 失败: ${written.error || "unknown"}`);
  }
  throw new Error(`提交 manifest 失败（CAS 重试耗尽）: ${sessionId}`);
}

/** 读取远端全量会话（manifest + 所有 segment），校验每个 segment 的 hash。 */
export async function readSession(store, sessionId) {
  const manifest = await readManifest(store, sessionId);
  if (!manifest) return null;
  const entries = [];
  const segmentBodies = [];
  for (const seg of manifest.segments) {
    const r = await store.get(seg.key);
    if (!r.ok) throw new Error(`读取 segment 失败 (${seg.key}): ${r.error}`);
    const h = `sha256:${sha256(r.body)}`;
    if (h !== seg.hash) throw new Error(`segment hash 不匹配: ${seg.key}`);
    segmentBodies.push(r.body.endsWith("\n") ? r.body : r.body + "\n");
    const segEntries = [];
    for (const line of r.body.split(/\r?\n/).filter(Boolean)) {
      let obj;
      try { obj = JSON.parse(line); } catch (e) { throw new Error(`segment 行损坏 (${seg.key}): ${e.message}`); }
      segEntries.push({ obj, id: obj.id, line, parentId: obj.parentId ?? null });
    }
    if (Array.isArray(seg.entryIds) && seg.entryIds.join("|") !== segEntries.map((e) => e.id).join("|")) {
      throw new Error(`segment entryIds 与内容不一致: ${seg.key}`);
    }
    entries.push(...segEntries);
  }
  if (manifest.head.seq !== entries.length) {
    throw new Error(`manifest head=${manifest.head.seq} 与 segment entry 数=${entries.length} 不一致`);
  }
  if (entries.length > 0) {
    const last = entries[entries.length - 1];
    if (last.id !== manifest.head.entryId || hashLine(last.line) !== manifest.head.hash) {
      throw new Error("manifest head 与最后一个 entry 不一致");
    }
  }
  return {
    manifest,
    header: manifest.header,
    entries,
    text: renderJsonl(manifest.header, entries),
    segmentBodies,
  };
}

function atomicWrite(path, text) {
  mkdirSync(dirname(path), { recursive: true });
  const tmp = `${path}.tmp-${process.pid}-${Date.now()}`;
  writeFileSync(tmp, text, { encoding: "utf8", mode: 0o600 });
  renameSync(tmp, path);
}

/**
 * 把远端会话物化到本地。
 * 本地领先（outbox 未刷）时保留本地，needsAppend=true；本地落后/缺失时用远端覆盖。
 */
export async function materialize(store, sessionId, localFile, opts = {}) {
  const manifest = await readManifest(store, sessionId);
  if (!manifest) return null;
  const remote = await readSession(store, sessionId);
  if (existsSync(localFile) && !opts.force) {
    let local;
    try { local = parseLocalJsonl(readLocalFile(localFile).body); }
    catch (e) { local = null; }
    if (local) {
      if (localPrefixMatches(manifest, local)) {
        if (local.entries.length >= manifest.head.seq) {
          return { manifest, local: true, needsAppend: local.entries.length > manifest.head.seq, remote };
        }
      } else if (local.entries.length > 0 && local.entries.length < manifest.head.seq) {
        // 本地是远端的旧前缀：可以安全覆盖
      } else if (local.entries.length > 0) {
        throw new LineageMismatchError(sessionId, "本地文件与远端不是同一谱系");
      }
    }
  }
  atomicWrite(localFile, remote.text);
  return { manifest, local: false, needsAppend: false, remote };
}

export async function listSessions(store) {
  const r = await store.list("sessions/");
  if (!r.ok) throw new Error(`列举会话失败: ${r.error}`);
  const ids = [];
  const legacy = new Set();
  for (const item of r.keys) {
    const key = item.key;
    const m = /^sessions\/([^/]+)\/manifest\.json$/.exec(key);
    if (m) { ids.push({ id: m[1], modified: item.lastModified, size: item.size }); continue; }
    const l = /^sessions\/([^/]+)\.(jsonl|json)$/.exec(key);
    if (l) legacy.add(l[1]);
  }
  const sessions = [];
  for (const item of ids.slice(0, 200)) {
    try {
      const manifest = await readManifest(store, item.id);
      if (manifest) sessions.push({
        id: item.id,
        updatedAt: manifest.updatedAt,
        seq: manifest.head.seq,
        segments: manifest.segments.length,
        tokens: manifest.stats?.tokens || 0,
        cost: manifest.stats?.cost || 0,
      });
    } catch (e) {
      if (/encryption-key-required/.test(String(e.message))) throw e;
      /* 损坏的 manifest 在 list 中跳过，doctor/status 会报 */
    }
  }
  sessions.sort((a, b) => String(b.updatedAt).localeCompare(String(a.updatedAt)));
  return { sessions, legacy: [...legacy].filter((id) => !sessions.some((s) => s.id === id)) };
}

export async function deleteSession(store, sessionId) {
  const r = await store.list(sessionPrefix(sessionId));
  if (!r.ok) throw new Error(`列举会话对象失败: ${r.error}`);
  let deleted = 0;
  for (const item of r.keys) {
    const d = await store.delete(item.key);
    if (!d.ok) throw new Error(`删除 ${item.key} 失败: ${d.error}`);
    deleted += 1;
  }
  for (const key of [legacyJsonlKey(sessionId), legacyTurnsKey(sessionId)]) {
    const d = await store.delete(key);
    if (d.ok) deleted += 1;
  }
  return deleted;
}

/**
 * 旧格式一次性导入：优先 sessions/<id>.jsonl，其次旧 turns JSON。
 * 旧对象不删除；目标 manifest 已存在则拒绝。
 */
export async function importV0(store, sessionId, localFile, opts = {}) {
  const existing = await readManifest(store, sessionId);
  if (existing) throw new Error(`会话 ${sessionId} 已是 v2（manifest 存在），无需导入`);
  const lease = opts.lease;
  if (lease) await lease.acquire(sessionId);
  let text = null;
  const oldJsonl = await store.get(legacyJsonlKey(sessionId));
  if (oldJsonl.ok) {
    const clean = sanitizeLocalJsonl(oldJsonl.body);
    if (!clean) throw new Error(`旧 JSONL 损坏: ${legacyJsonlKey(sessionId)}`);
    text = clean.body;
  } else if (!/not-found|NoSuchKey|\b404\b/i.test(String(oldJsonl.error))) {
    throw new Error(`读取旧 JSONL 失败: ${oldJsonl.error}`);
  }
  if (!text) {
    const oldTurns = await store.get(legacyTurnsKey(sessionId));
    if (!oldTurns.ok) {
      throw new Error(/not-found|NoSuchKey|\b404\b/i.test(String(oldTurns.error))
        ? `会话不存在（既无 manifest，也无旧 .jsonl/.json）`
        : `读取旧 turns JSON 失败: ${oldTurns.error}`);
    }
    let parsed;
    try { parsed = JSON.parse(oldTurns.body); } catch (e) { throw new Error(`旧 turns JSON 损坏: ${e.message}`); }
    const turns = Array.isArray(parsed.turns) ? parsed.turns : [];
    if (turns.length === 0) throw new Error("旧会话为空，拒绝导入");
    const converted = jsonlFromTurns(sessionId, turns, { cwd: opts.cwd || process.cwd() });
    text = converted.jsonl;
  }
  const local = parseLocalJsonl(text);
  atomicWrite(localFile, text);
  const manifest = await appendFromFile(store, sessionId, localFile, { lease, now: opts.now });
  return { manifest, text, entries: local.entries.length };
}
