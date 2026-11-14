// v2/adapter.js — 对象存储适配器（BOS/S3 经 aws CLI；测试用内存 store）
import { bosGet, bosPut, bosDelete, bosList } from "../bos.js";
import { createCrypto } from "./crypto.js";

export function createBosAdapter(cfg, deps = {}) {
  const crypto = deps.crypto || createCrypto();
  const opts = {
    bucket: cfg.bucket,
    endpoint: cfg.endpoint,
    profile: cfg.profile,
    region: cfg.region,
  };
  const get = deps.get || ((key, o) => bosGet(key, o));
  const put = deps.put || ((key, body, o) => bosPut(key, body, o));
  const del = deps.del || ((key, o) => bosDelete(key, o));
  const list = deps.list || ((prefix, o) => bosList(prefix, o));

  return {
    crypto,
    opts,
    async get(key) {
      const r = await get(key, opts);
      if (!r.ok) return r;
      if (!crypto.enabled) {
        try {
          const probe = JSON.parse(r.body);
          if (probe?.v === 1 && probe?.alg === "aes-256-gcm") {
            return { ok: false, error: "encryption-key-required" };
          }
        } catch (e) { /* 明文 */ }
      }
      try {
        return { ...r, body: crypto.decrypt(r.body) };
      } catch (e) {
        return { ok: false, error: `decrypt-failed: ${e.message}` };
      }
    },
    async put(key, body, extra = {}) {
      return put(key, crypto.encrypt(body), { ...opts, ...extra });
    },
    async delete(key) {
      return del(key, opts);
    },
    async list(prefix) {
      return list(prefix, opts);
    },
  };
}

export function createMemoryAdapter({ crypto } = {}) {
  const objects = new Map();
  let n = 0;
  const c = crypto || createCrypto("");
  return {
    crypto: c,
    objects,
    async get(key) {
      const cur = objects.get(key);
      if (!cur) return { ok: false, error: "not-found" };
      return { ok: true, body: c.decrypt(cur.body), etag: cur.etag };
    },
    async put(key, body, extra = {}) {
      const stored = c.encrypt(body);
      const cur = objects.get(key);
      if (extra.ifNoneMatch && cur) return { ok: false, conflict: true, error: "conflict" };
      if (extra.ifMatch && (!cur || cur.etag !== extra.ifMatch)) {
        return { ok: false, conflict: true, error: "conflict" };
      }
      n += 1;
      const etag = `"etag-${n}"`;
      objects.set(key, { body: stored, etag });
      return { ok: true, result: { ETag: etag } };
    },
    async delete(key) {
      objects.delete(key);
      return { ok: true };
    },
    async list(prefix) {
      const keys = [...objects.keys()]
        .filter((k) => k.startsWith(prefix))
        .map((key) => ({ key, size: objects.get(key).body.length }));
      return { ok: true, keys };
    },
  };
}
