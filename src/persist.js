// persist.js — 会话权威写/恢复 (BOS 队列 + CAS 门禁 + BOS-first)
// TUI 只编排; 本模块可注入 get/put/probe, 单测不需要 aws CLI
import { homedir } from "node:os";
import { join } from "node:path";
import { existsSync, readFileSync } from "node:fs";
import { bosGet, bosPut, casGateSticky, probeStoreCas, resolveEndpoint, resolveRegion } from "./bos.js";

export const BOS_QUEUE_MAX = 50;

export function storeFromCfg(cfg) {
  const endpoint = resolveEndpoint(cfg?.persistence?.endpoint);
  const region = resolveRegion(endpoint, cfg?.persistence?.region);
  const profile = String(cfg?.persistence?.profile || "").trim() || "bos";
  return {
    bucket: cfg?.persistence?.bucket || null,
    endpoint,
    region,
    profile,
    awsExtra: { AWS_PROFILE: profile, ...(region ? { AWS_REGION: region } : {}) },
  };
}

export function defaultLoadStore() {
  const f = join(homedir(), ".config", "celagent", "settings.json");
  if (!existsSync(f)) {
    const err = new Error("no-config");
    err.code = "no-config";
    throw err;
  }
  return storeFromCfg(JSON.parse(readFileSync(f, "utf8")));
}

/** not-found / conflict / fatal(不重试) / transient(队列留队重试) */
export function classifyStoreError(error) {
  if (error == null || error === "") return "transient";
  const msg = String(error);
  if (error === "not-found" || /\bNoSuchKey\b|\b404\b/i.test(msg)) return "not-found";
  if (error === "conflict" || /\bPreconditionFailed\b|\b412\b/i.test(msg)) return "conflict";
  if (error === "endpoint-not-allowed" || error === "no-bucket" || error === "no-config") return "fatal";
  if (
    /AccessDenied|InvalidAccessKeyId|SignatureDoesNotMatch|ExpiredToken|PermanentRedirect|InvalidBucketName|NoSuchBucket|endpoint-not-allowed|\b403\b|\b401\b/i.test(msg)
  ) {
    return "fatal";
  }
  return "transient";
}

export function makeTurnEntry(sessionId, seq, role, msg, fullContent, fullToolResults) {
  const entry = { turn: seq, role, msg, ts: Date.now() };
  if (fullContent && fullContent.length > 0) entry.content = fullContent;
  if (fullToolResults && fullToolResults.length > 0) entry.toolResults = fullToolResults;
  return entry;
}

export function mergeTurn(session, seq, role, msg, fullContent, fullToolResults) {
  let finalSeq = seq;
  if (session.turns.length > 0) {
    const exists = session.turns.some((t) => t.turn === seq);
    if (!exists) {
      const nums = session.turns.map((t) => Number(t.turn)).filter((n) => Number.isFinite(n));
      const maxTurn = nums.length ? Math.max(...nums) : 0;
      finalSeq = maxTurn + 1;
    }
  }
  const entry = makeTurnEntry(session.id, finalSeq, role, msg, fullContent, fullToolResults);
  const idx = session.turns.findIndex((t) => t.turn === finalSeq);
  if (idx >= 0) {
    const prev = session.turns[idx];
    if (prev?.content && !(fullContent && fullContent.length)) entry.content = prev.content;
    if (prev?.toolResults && !(fullToolResults && fullToolResults.length)) entry.toolResults = prev.toolResults;
    session.turns[idx] = entry;
  } else {
    session.turns.push(entry);
  }
  session.updatedAt = Date.now();
}

export function storeCasKey(store) {
  return [store.endpoint, store.bucket, store.profile, store.region || ""].join("|");
}

function defaultWarn(ch, msg) {
  console.warn(msg);
}

function defaultSleep(ms) {
  return new Promise((r) => {
    const t = setTimeout(r, ms);
    if (t.unref) t.unref();
  });
}

export function createPersister(deps = {}) {
  const warn = deps.warn || defaultWarn;
  const sleep = deps.sleep || defaultSleep;
  const get = deps.get || ((key, opts) => bosGet(key, opts));
  const put = deps.put || ((key, content, opts) => bosPut(key, content, opts));
  const probe = deps.probe || ((opts) => probeStoreCas(opts));
  const loadStore = deps.loadStore || defaultLoadStore;

  const bosPending = [];
  let bosPumping = false;
  let bosQueue = Promise.resolve();
  let casGateCache = { key: null, result: null };
  let casGateInflight = { key: null, promise: null };

  async function ensureStoreCas(store) {
    const key = storeCasKey(store);
    if (casGateCache.key === key && casGateCache.result) return casGateCache.result;
    if (casGateInflight.key === key && casGateInflight.promise) return casGateInflight.promise;
    const promise = (async () => {
      try {
        const r = await probe({
          bucket: store.bucket,
          endpoint: store.endpoint,
          profile: store.profile,
          region: store.region,
        });
        if (casGateSticky(r)) casGateCache = { key, result: r };
        return r;
      } finally {
        if (casGateInflight.promise === promise) casGateInflight = { key: null, promise: null };
      }
    })();
    casGateInflight = { key, promise };
    return promise;
  }

  async function persistTurnToBos(sessionId, seq, role, msg, fullContent, fullToolResults) {
    let store;
    try {
      store = deps.store || loadStore();
    } catch (e) {
      if (e.code === "endpoint-not-allowed") {
        warn("persist", `  (警告: ${e.message})`);
        return;
      }
      if (e.code === "no-config") {
        warn("persist", "  (警告: 未配置 persistence.bucket, 会话不会持久化)");
        return;
      }
      warn("persist", `  (警告: ${e.message})`);
      return classifyStoreError(e.message) === "transient" ? "retry" : undefined;
    }
    const bucket = store.bucket;
    const endpoint = store.endpoint;
    const profile = store.profile;
    const region = store.region;
    if (!bucket) {
      warn("persist", "  (警告: 未配置 persistence.bucket, 会话不会持久化)");
      return;
    }
    const cas = await ensureStoreCas(store);
    if (!cas.ok) {
      if (cas.transient) {
        warn("cas-temp", `  (警告: CAS 探针暂未通过, 本轮写入将重试: ${cas.message || cas.error})`);
        return "retry";
      }
      warn("cas", `  (警告: 此存储不能保证 RPO=0,拒绝权威写入: ${cas.message || cas.error})`);
      return;
    }
    const key = `sessions/${sessionId}.json`;
    const common = { bucket, endpoint, profile, region };
    for (let attempt = 0; attempt < 3; attempt++) {
      let session = { id: sessionId, turns: [] };
      let etag = undefined;
      const existing = await get(key, common);
      if (existing.ok) {
        try {
          session = JSON.parse(existing.body);
          if (!Array.isArray(session.turns)) session.turns = [];
        } catch (e) {
          warn("persist", `  (警告: BOS 会话 JSON 损坏, 跳过本轮以免覆盖历史: ${sessionId})`);
          return;
        }
        etag = existing.etag;
      } else if (classifyStoreError(existing.error) === "not-found") {
        session.turns.push(makeTurnEntry(sessionId, seq, role, msg, fullContent, fullToolResults));
        session.updatedAt = Date.now();
        const created = await put(key, session, { ...common, ifNoneMatch: true });
        if (created.ok) return;
        if (created.conflict) {
          await sleep(100);
          continue;
        }
        const kind = classifyStoreError(created.error);
        if (kind === "transient") return "retry";
        warn("persist", `  (警告: BOS 首写失败: ${created.error || "未知错误"})`);
        return;
      } else {
        const kind = classifyStoreError(existing.error);
        if (kind === "transient") return "retry";
        warn("persist", `  (警告: BOS 读取失败, 跳过本轮持久化: ${existing.error})`);
        return;
      }
      mergeTurn(session, seq, role, msg, fullContent, fullToolResults);
      const written = await put(key, session, { ...common, ifMatch: etag });
      if (written.ok) return;
      if (written.conflict) {
        await sleep(100);
        continue;
      }
      const kind = classifyStoreError(written.error);
      if (kind === "transient") return "retry";
      warn("persist", `  (警告: BOS 持久化失败: ${written.error || "未知错误"})`);
      return;
    }
    // 3 次 CAS 冲突仍未写入 — 留队, 对端写完后再合并
    return "retry";
  }

  function pumpBosQueue() {
    if (bosPumping) return;
    bosPumping = true;
    bosQueue = (async () => {
      let retries = 0;
      try {
        while (bosPending.length) {
          const job = bosPending[0];
          let outcome;
          try {
            outcome = await persistTurnToBos(job.sessionId, job.seq, job.role, job.msg, job.fullContent, job.fullToolResults);
          } catch (e) {
            warn("persist", `  (警告: BOS 持久化异常: ${e.message})`);
            const kind = classifyStoreError(e.message);
            if (kind === "transient") outcome = "retry";
          }
          if (outcome === "retry") {
            retries += 1;
            const delay = Math.min(60000, 1000 * 2 ** Math.min(retries, 6));
            await sleep(delay);
            continue;
          }
          retries = 0;
          if (bosPending[0] === job) bosPending.shift();
        }
      } finally {
        bosPumping = false;
        if (bosPending.length) pumpBosQueue();
      }
    })();
  }

  function queueBosWrite(sessionId, seq, role, msg, opts = {}) {
    const { fullContent = null, fullToolResults = null } = opts || {};
    if (bosPending.length >= BOS_QUEUE_MAX) {
      bosPending.shift();
      warn("queue", "  (警告: BOS 写队列过长, 丢弃最旧任务)");
    }
    bosPending.push({ sessionId, seq, role, msg, fullContent, fullToolResults });
    pumpBosQueue();
    return bosQueue;
  }

  async function flush(timeoutMs = 10000) {
    pumpBosQueue();
    await Promise.race([
      bosQueue,
      new Promise((r) => {
        const t = setTimeout(r, timeoutMs);
        if (t.unref) t.unref();
      }),
    ]).catch(() => {});
  }

  return {
    persistTurnToBos,
    queueBosWrite,
    flush,
    ensureStoreCas,
    get pending() {
      return bosPending;
    },
    get queue() {
      return bosQueue;
    },
  };
}

const defaultPersister = createPersister();
export const persistTurnToBos = (...args) => defaultPersister.persistTurnToBos(...args);
export const queueBosWrite = (...args) => defaultPersister.queueBosWrite(...args);
export const flushBosQueue = (...args) => defaultPersister.flush(...args);
export const getBosQueue = () => defaultPersister.queue;

/**
 * BOS-first 恢复。
 * - BOS 读成功 → { source:"bos", turns }
 * - BOS not-found / 无 bucket → 才调用 fallbackResume (worker 缓存)
 * - BOS 瞬时/权限失败 → 不回退 (避免 8000 字截断被当成完整历史)
 * - JSON 损坏 → 不回退、不覆盖
 */
export async function loadSessionHistory(sessionId, opts = {}) {
  const get = opts.get || ((key, o) => bosGet(key, o));
  const fallbackResume = opts.fallbackResume;
  let store;
  try {
    store = opts.store || (opts.loadStore || defaultLoadStore)();
  } catch (e) {
    if (e.code === "endpoint-not-allowed") {
      return { turns: null, source: null, error: e.message, fatal: true };
    }
    // 无配置: 允许 worker miss 回退
    if (fallbackResume) {
      try {
        const turns = await fallbackResume(sessionId);
        if (turns?.length) return { turns, source: "worker", miss: true };
      } catch (e2) { /* ignore */ }
    }
    return { turns: null, source: null, error: e.message || "no-config" };
  }
  if (store.bucket) {
    const existing = await get(`sessions/${sessionId}.json`, {
      bucket: store.bucket,
      endpoint: store.endpoint,
      profile: store.profile,
      region: store.region,
    });
    if (existing.ok) {
      try {
        const session = JSON.parse(existing.body);
        return { turns: session.turns || [], source: "bos" };
      } catch (e) {
        return { turns: null, source: "bos", error: "corrupt", corrupt: true };
      }
    }
    const kind = classifyStoreError(existing.error);
    if (kind !== "not-found") {
      return {
        turns: null,
        source: "bos",
        error: existing.error,
        transient: kind === "transient",
        fatal: kind === "fatal",
      };
    }
  }
  if (fallbackResume) {
    try {
      const turns = await fallbackResume(sessionId);
      if (turns?.length) return { turns, source: "worker", miss: true };
    } catch (e) { /* ignore */ }
  }
  return { turns: store.bucket ? [] : null, source: store.bucket ? "bos" : null, miss: true };
}
