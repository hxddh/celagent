// v2/lease.js — 单写者租约 + epoch fencing
// 租约对象独立于 manifest；写入前 assert，接管时 epoch 递增，旧写者的 CAS 会失败。
import { hostname } from "node:os";
import { leaseKey } from "./keys.js";

export class SessionLockedError extends Error {
  constructor(sessionId, lease) {
    const who = lease ? `${lease.owner} (epoch ${lease.epoch})` : "unknown";
    super(`会话 ${sessionId} 正被 ${who} 持有；先在该机器退出，或等待租约过期后接管`);
    this.name = "SessionLockedError";
    this.code = "session-locked";
    this.sessionId = sessionId;
    this.lease = lease || null;
  }
}

export class LeaseLostError extends Error {
  constructor(sessionId, detail = "") {
    super(`会话 ${sessionId} 的写租约已丢失${detail ? `：${detail}` : ""}`);
    this.name = "LeaseLostError";
    this.code = "lease-lost";
    this.sessionId = sessionId;
  }
}

function ownerAlive(owner) {
  const m = /^(.*)-(\d+)$/.exec(String(owner || ""));
  if (!m) return true;
  if (m[1] !== hostname()) return true;
  try { process.kill(Number(m[2]), 0); return true; }
  catch (e) { return e.code === "EPERM"; }
}

export function createLeaseManager(store, opts = {}) {
  const now = opts.now || Date.now;
  const owner = opts.owner || `${hostname()}-${process.pid}`;
  const ttlMs = opts.ttlMs || 15000;
  const held = new Map(); // sessionId -> epoch

  async function read(sessionId) {
    const r = await store.get(leaseKey(sessionId));
    if (!r.ok) {
      if (/not-found|NoSuchKey|\b404\b/i.test(String(r.error))) return { lease: null, etag: null };
      throw new Error(`读取租约失败: ${r.error}`);
    }
    try {
      const lease = JSON.parse(r.body);
      return { lease, etag: r.etag };
    } catch (e) {
      throw new Error(`租约对象损坏: ${e.message}`);
    }
  }

  async function acquire(sessionId) {
    for (let attempt = 0; attempt < 6; attempt++) {
      const { lease, etag } = await read(sessionId);
      if (!lease) {
        const fresh = { owner, epoch: 1, expiresAt: now() + ttlMs };
        const put = await store.put(leaseKey(sessionId), JSON.stringify(fresh), { ifNoneMatch: true });
        if (put.ok) { held.set(sessionId, fresh.epoch); return fresh; }
        if (put.conflict) continue;
        throw new Error(`获取租约失败: ${put.error}`);
      }
      if (lease.owner === owner) {
        const renewed = { ...lease, expiresAt: now() + ttlMs };
        const put = await store.put(leaseKey(sessionId), JSON.stringify(renewed), { ifMatch: etag });
        if (put.ok) { held.set(sessionId, renewed.epoch); return renewed; }
        if (put.conflict) continue;
        throw new Error(`续租失败: ${put.error}`);
      }
      if (Number(lease.expiresAt) > now() && ownerAlive(lease.owner)) throw new SessionLockedError(sessionId, lease);
      const taken = { owner, epoch: Number(lease.epoch || 0) + 1, expiresAt: now() + ttlMs };
      const put = await store.put(leaseKey(sessionId), JSON.stringify(taken), { ifMatch: etag });
      if (put.ok) { held.set(sessionId, taken.epoch); return taken; }
      if (put.conflict) continue;
      throw new Error(`接管租约失败: ${put.error}`);
    }
    throw new Error(`获取租约失败（CAS 冲突重试耗尽）: ${sessionId}`);
  }

  async function assert(sessionId) {
    const { lease } = await read(sessionId);
    const epoch = held.get(sessionId);
    if (!lease) throw new LeaseLostError(sessionId, "对象不存在");
    if (lease.owner !== owner) throw new LeaseLostError(sessionId, `owner=${lease.owner}`);
    if (Number(lease.expiresAt) <= now()) throw new LeaseLostError(sessionId, "已过期");
    if (epoch !== undefined && Number(lease.epoch) !== Number(epoch)) {
      throw new LeaseLostError(sessionId, `epoch ${epoch} -> ${lease.epoch}`);
    }
    return lease;
  }

  async function renew(sessionId) {
    const { lease, etag } = await read(sessionId);
    if (!lease || lease.owner !== owner) throw new LeaseLostError(sessionId, "无法续租");
    const next = { ...lease, expiresAt: now() + ttlMs };
    const put = await store.put(leaseKey(sessionId), JSON.stringify(next), { ifMatch: etag });
    if (!put.ok) throw new LeaseLostError(sessionId, put.error || "续租 CAS 失败");
    held.set(sessionId, next.epoch);
    return next;
  }

  async function release(sessionId) {
    const { lease } = await read(sessionId).catch(() => ({ lease: null }));
    held.delete(sessionId);
    if (!lease || lease.owner !== owner) return false;
    const del = await store.delete(leaseKey(sessionId));
    return Boolean(del.ok);
  }

  function startHeartbeat(sessionId, onLost) {
    const t = setInterval(() => {
      renew(sessionId).catch((e) => {
        clearInterval(t);
        if (onLost) onLost(e);
      });
    }, Math.max(1000, Math.floor(ttlMs / 3)));
    t.unref?.();
    return () => clearInterval(t);
  }

  return { owner, ttlMs, acquire, assert, renew, release, startHeartbeat, read };
}
