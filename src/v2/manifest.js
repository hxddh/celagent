// v2/manifest.js — manifest 的创建、读取、校验与 CAS 写入
import { manifestKey } from "./keys.js";

export const MANIFEST_VERSION = 2;

export class ManifestCorruptError extends Error {
  constructor(sessionId, detail) {
    super(`会话 ${sessionId} 的 manifest 损坏: ${detail}`);
    this.name = "ManifestCorruptError";
    this.code = "manifest-corrupt";
  }
}

export function createManifest(sessionId, header) {
  const now = new Date().toISOString();
  return {
    v: MANIFEST_VERSION,
    session: sessionId,
    schema: 3,
    createdAt: now,
    updatedAt: now,
    header,
    head: { seq: 0, entryId: null, hash: null },
    segments: [],
    workspace: null,
    stats: { tokens: 0, cost: 0 },
  };
}

export function validateManifest(sessionId, m) {
  if (!m || typeof m !== "object") throw new ManifestCorruptError(sessionId, "不是对象");
  if (m.v !== MANIFEST_VERSION) throw new ManifestCorruptError(sessionId, `不支持的版本 ${m.v}`);
  if (m.session !== sessionId) throw new ManifestCorruptError(sessionId, `session 字段不匹配`);
  if (!m.header || m.header.type !== "session") throw new ManifestCorruptError(sessionId, "缺少 session header");
  if (!m.head || typeof m.head.seq !== "number") throw new ManifestCorruptError(sessionId, "缺少 head");
  if (!Array.isArray(m.segments)) throw new ManifestCorruptError(sessionId, "segments 不是数组");
  return m;
}

export async function readManifest(store, sessionId) {
  const r = await store.get(manifestKey(sessionId));
  if (!r.ok) {
    if (/not-found|NoSuchKey|\b404\b/i.test(String(r.error))) return null;
    throw new Error(`读取 manifest 失败: ${r.error}`);
  }
  let m;
  try {
    m = JSON.parse(r.body);
  } catch (e) {
    throw new ManifestCorruptError(sessionId, e.message);
  }
  validateManifest(sessionId, m);
  m.etag = r.etag;
  return m;
}

export async function writeManifest(store, manifest, etag) {
  const body = JSON.stringify({ ...manifest, etag: undefined }, null, 2);
  const opts = etag ? { ifMatch: etag } : { ifNoneMatch: true };
  const r = await store.put(manifestKey(manifest.session), body, opts);
  if (r.ok) {
    let nextEtag = r.result?.ETag;
    if (!nextEtag) {
      const g = await store.get(manifestKey(manifest.session));
      if (g.ok) nextEtag = g.etag;
    }
    const next = { ...manifest, etag: nextEtag };
    return { ok: true, manifest: next };
  }
  if (r.conflict) return { ok: false, conflict: true };
  return { ok: false, error: r.error || "unknown" };
}
